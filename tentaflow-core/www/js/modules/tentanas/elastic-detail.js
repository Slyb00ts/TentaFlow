// =============================================================================
// Plik: modules/tentanas/elastic-detail.js
// Opis: Karta Elastic Array ze stanem, montowaniami, historią SnapRAID i zaawansowanym przenoszeniem z cache.
// Przykład: drawElasticDetail(screen, body) korzysta z nazwy screen.array.
// =============================================================================

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, poolCrumbTail, sprite, fmtOptionalBytes, fmtBytes, fmtAgo, pct, fmtDate, fmtDuration, fmtSchedule, errMessage, healthClass, KIND_BADGE, POLL_POOLS_MS, ADMIN_TIMEOUT_MS, wordReasons, nodeTextTitle, jobKindLabel } from '/js/modules/tentanas/format.js';
import { setAttr, setText, patchKeyedList, paintStatCards, SLOT, slotEl, setClass } from '/js/lib/dom-patch.js';
import { openRetypeDialog } from '/js/lib/retype-dialog.js';
import { followResponse, dangerRowHtml, warningHtml, NAS_DIALOG } from '/js/modules/tentanas/dialogs.js';
import { openScheduleEditor, scheduleFieldsHtml, wireScheduleFields, readScheduleFields } from '/js/modules/tentanas/schedule-editor.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-window.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';

const knownBytes = (value) => value != null && Number.isFinite(Number(value)) && Number(value) >= 0;
const row = (label, value) => `<div class="sr"><span class="k">${escapeHtml(label)}</span><span class="v">${escapeHtml(value)}</span></div>`;
const triState = (value) => value === true ? T('elastic.yes') : value === false ? T('elastic.no') : T('elastic.unknown');

// A member's part in the array, in words: "dysk danych 2", "dysk cache",
// "dysk parity 1". The wire's `name` is the SLOT (`d2`, `c1`, `parity1`) —
// what the branch directory is called and what a request names the member
// by — which makes it a key, not something to print. A parity entry carries
// its 1-based `index`; a data slot carries its ordinal as the trailing digits.
function memberPart(disk) {
  if (disk.index != null) return T('elastic.member_parity', { n: disk.index });
  if (disk.role === 'cache') return T('elastic.member_cache');
  const n = /(\d+)$/.exec(String(disk.name || ''))?.[1] || '';
  return T('elastic.member_data', { n });
}

// What every sentence calls a member: its LIVE kernel name (`sdg` — the
// node's inventory, or the helper's own observation of the member's device in
// the same read), or its part in the array when nothing names the device now.
// The name it was last seen under (`diskLastName`) is never used here: a
// sentence would present it as the current device, and the kernel may have
// handed that name to another disk since. The cell shows it, marked.
export function memberName(disk) {
  return String(disk?.diskName || '').trim() || memberPart(disk);
}

// One entry per mutation the detail can send, so a new action cannot reach the
// transport without also naming the sentence the admin sees for it.
const ACTION_REQUEST = { restore: 'tentaNasElasticArrayRestoreRequest', sync: 'tentaNasElasticArraySyncRequest', scrub: 'tentaNasElasticArrayScrubRequest', mover: 'tentaNasElasticArrayMoverRequest' };
const ACTION_TITLE = { restore: 'elastic.restore', sync: 'elastic.sync_now', scrub: 'elastic.scrub_now', mover: 'elastic.mover_run_now' };
const ACTION_ACCEPTED = { restore: 'elastic.job_running', sync: 'elastic.maintenance_accepted', scrub: 'elastic.maintenance_accepted', mover: 'elastic.mover_accepted' };
const ACTION_APPROVAL = { restore: 'elastic.approval', sync: 'elastic.maintenance_approval', scrub: 'elastic.maintenance_approval', mover: 'elastic.mover_approval' };

// Why a repair CANNOT run right now, or '' when the array is in a shape
// `snapraid fix` can work on.
//
// It mirrors `tentanas::elastic::repair_blocker`. A repair writes back the
// blocks a scrub marked bad, and it needs every data disk present and mounted
// to do it — which makes a dead data disk, the very situation a repair is
// reached for, the one situation in which it cannot run. Disk replacement is
// withdrawn (round 4), so the screen says what has to happen first instead of
// offering a button whose only possible outcome is the helper's own
// `precondition_failed`.
//
// `devicePresent` and `mounted` are tri-states and only `false` counts: `null`
// is "nothing looked", the state of every array on a node whose disks have not
// been read yet, and treating it as absence would block every array.
function repairBlocker(array) {
  for (const member of array?.dataDisks || []) {
    if (member.devicePresent === false) return T('elastic.repair_disk_absent', { disk: memberName(member) });
    if (member.mounted === false) return T('elastic.repair_disk_unmounted', { disk: memberName(member) });
  }
  return '';
}

// Why the helper would refuse a repair whatever the history says: an add is
// unfinished, or the array waits for a Restore. It mirrors the cause half of
// `tentanas::elastic::repair_blocker`.
function repairHeld(array) {
  if (array?.attention === 'add_disk') return T('elastic.repair_add_pending');
  if (array?.attention === 'other') return T('elastic.repair_needs_restore');
  return '';
}

// What a repair would be working on, or '' when the array reports nothing a
// repair could recover.
//
// It MIRRORS `tentanas::elastic::repair_evidence` FIELD FOR FIELD, over the
// same object: the node runs that rule on the very `NasElasticArray` this
// reads. A repair writes the named disk back from the parity checkpoint,
// so a button on an array that reports nothing wrong would only overwrite
// healthy data — and a button the node then refuses is worse still, because
// the admin reads the refusal as a fault.
//
// The evidence is about PARITY, and `unresolvedOperation` is deliberately not
// read: it is equally true for a mover that stopped part-way and for an
// add-disk that failed, neither of which parity can repair. So the history is
// walked instead — newest first, so a successful repair seen BEFORE an
// unsuccessful run is one that came after it and settled it, which is the rule
// the node applies in SQL over the same rows.
function repairEvidence(array) {
  // What ENDS the fault is its cure (M2 of the release review): a
  // successful repair, a clean full scrub, or for file errors alone the Sync
  // that drops them — `tentanas::elastic::unresolved_parity_fault`.
  let syncedSince = false;
  for (const run of array?.snapraid?.history || []) {
    if (['fix', 'scrub'].includes(run.kind) && run.outcome === 'ok') break;
    if (run.kind === 'sync' && run.outcome === 'ok') syncedSince = true;
    // ONLY A SCRUB THAT COUNTED ERRORS. A repair writes back the blocks
    // snapraid MARKED as bad, and a scrub is what marks them: measured on
    // rig11, `-e fix` with no scrub behind it writes nothing at all and still
    // reports "Everything OK". A failed sync marks nothing, and the parity
    // figure of the reporting window is not a mark in the content file.
    if (run.kind === 'scrub' && ['failed', 'needs_attention'].includes(run.outcome)
      && typeof run.errors === 'number' && run.errors > 0) {
      if (syncedSince && run.errorsData === 0) break;
      return T('elastic.repair_reason_scrub_errors', { n: run.errors });
    }
  }
  return '';
}

// A Repair that failed and that no later Repair or full Scrub settled —
// newest first, the rule `tentanas::elastic::sync_acknowledgement_needed`
// applies over the same rows.
function unresolvedFixFault(array) {
  for (const run of array?.snapraid?.history || []) {
    if (['fix', 'scrub'].includes(run.kind) && run.outcome === 'ok') return false;
    if (run.kind === 'fix' && ['failed', 'needs_attention'].includes(run.outcome)) return true;
  }
  return false;
}

// Whether a Sync on this array needs the admin's confirm: the node says so
// (`syncNeedsAcknowledgement`, which carries the helper's own recorded cause),
// or the history shows an unrepaired Scrub or Repair fault. Measured on rig11
// (M3, design repository reviews/artifacts/elastic-measurements-2026-09-24):
// a Sync over such a fault drops no UNCHANGED file from the content file —
// unchanged files stay repairable, marked blocks and unreadable (EIO) ones
// alike. It does drop the files deleted since the last Sync, as every Sync
// does, and records the changed ones in their new state, damaged ones
// included; some cases stay unmeasured. So
// only the confirm that names that cost may send `acknowledgeParityFault`,
// and the node and its helper refuse the Sync without it.
export function syncNeedsAcknowledgement(array) {
  return array?.syncNeedsAcknowledgement === true || Boolean(repairEvidence(array)) || unresolvedFixFault(array);
}

// The helper's recorded cause, in the reader's language. A cause this build
// has no words for gets none: the screen never prints a code.
const ATTENTION_CAUSES = ['sync_failed', 'scrub_failed', 'fix_failed', 'add_disk', 'other'];
function attentionText(array) {
  return ATTENTION_CAUSES.includes(array?.attention) ? T(`elastic.attention_${array.attention}`) : '';
}

// The members a state sentence names, per role (`elastic::members_params`):
// comma-separated kernel names, or "#<n>" for a member the node knows only by
// its number — never its internal slot.
const STATE_MEMBER_ROLES = ['data', 'cache', 'parity'];
function stateMembers(p) {
  const out = [];
  for (const role of STATE_MEMBER_ROLES) {
    for (const name of String(p[role] || '').split(',').map((n) => n.trim()).filter(Boolean)) {
      const number = /^#(\d+)$/.exec(name)?.[1];
      out.push(number
        ? T(`elastic.state_member.${role}_number`, { n: number })
        : T(`elastic.state_member.${role}`, { name }));
    }
  }
  return out.join(', ');
}
const withMembers = (key) => (p) => {
  const members = stateMembers(p);
  return members ? T(key, { members }) : null;
};

// The array state's codes (`NasElasticArray::state_reasons`), in words.
const STATE_WORDS = new Map([
  ['mount_table_unknown', () => T('elastic.state_reason.mount_table_unknown')],
  ['mergerfs_missing', () => T('elastic.state_reason.mergerfs_missing')],
  ['snapraid_unusable', () => T('elastic.state_reason.snapraid_unusable')],
  ['branches_unknown', withMembers('elastic.state_reason.branches_unknown')],
  ['branches_mountable', withMembers('elastic.state_reason.branches_mountable')],
  ['branches_gone', (p) => {
    const members = stateMembers(p);
    if (!members || !['serving', 'down'].includes(p.union)) return null;
    return T(`elastic.state_reason.branches_gone_${p.union}`, { members });
  }],
  ['union_not_mounted', () => T('elastic.state_reason.union_not_mounted')],
  ['no_parity', () => T('elastic.state_reason.no_parity')],
  ['restart_required', () => T('elastic.state_reason.restart_required')],
  ['checkpoint_unfinished', () => T('elastic.state_reason.checkpoint_unfinished')],
  ['awaiting_confirmation', () => T('elastic.state_reason.awaiting_confirmation')],
  ['service_not_online', () => T('elastic.state_reason.service_not_online')],
  ['helper_failed', () => T('elastic.state_reason.helper_failed')],
  // The sentences the array's row STORES (migration 23): an operation's
  // error, named by the operation's kind, and a lost supervision.
  ['operation_failed', (p) => {
    const kind = p.operation === 'dissolve' ? 'destroy' : String(p.operation || '');
    const key = 'elastic_' + kind;
    const label = kind ? jobKindLabel(key) : key;
    return label !== key
      ? T('elastic.state_reason.operation_failed', { operation: label })
      : T('elastic.state_reason.operation_failed_unnamed');
  }],
  ['supervision_lost', () => T('elastic.state_reason.supervision_lost')],
]);

// The sentence that explains the array's state: the helper's recorded cause
// when there is one, otherwise the node's detail in the reader's language
// (its codes, wave 6), otherwise the node's own sentence as it came — a
// sentence the node stored without codes (a config import's reason) or a
// node too old to send them. The cause replaces the helper's raw text rather
// than sitting beside it — that text is the helper's, in one language, and
// may name what a screen must not show.
//
// An uncoded sentence (a row stored before migration 23 that no rule
// recognised, an older node) is shown only through the id filter: a stored
// error may name a by-id path or a WWN.
export function elasticStateDetail(array) {
  return attentionText(array) || wordReasons(array?.stateReasons, STATE_WORDS) || nodeTextTitle(array?.stateDetail);
}

// The node's own sentence, as the tooltip of the worded one — '' when the
// screen already shows that sentence itself.
export function elasticStateTitle(array) {
  const shown = elasticStateDetail(array);
  const own = nodeTextTitle(array?.stateDetail);
  return own && shown !== own ? own : '';
}

// The unfinished add, if the array has one: the disk's live name, or "nowy
// dysk" — never its id, and never the name it was last seen under presented
// as the device now.
const pendingAdd = (array) => array?.pendingAddDisk || null;
// A name it was LAST seen under is said as such, beside "nowy dysk", never
// as the device now.
const pendingAddName = (pending) => {
  const live = String(pending?.diskName || '').trim();
  if (live) return live;
  const last = String(pending?.diskLastName || '').trim();
  return last ? `${T('elastic.add_disk_new_disk')} (${T('elastic.last_seen_as', { name: last })})` : T('elastic.add_disk_new_disk');
};
const ADD_STEPS = ['format', 'mount', 'join', 'sync'];
const pendingStepText = (pending) => T(`elastic.add_disk_step_${ADD_STEPS.includes(pending?.step) ? pending.step : 'unknown'}`);

// Why a SnapRAID sync or scrub cannot be started on this array right now, or
// '' when it can. ONE rule for the detail pane's "Sync teraz"/"Scrub teraz"
// and the n05 card's "Sync teraz", so the two buttons can never disagree.
//
// A SYNC AND A FULL SCRUB ARE WHAT SETTLE a parity run that ended without
// success, so they are offered on the array such a run left behind —
// `parityRunAvailable` is the node's own admission rule, and reading `state`
// plus `unresolvedOperation` here instead disabled the one action that
// resolves that state (W5/W6 of the fourth review).
export function elasticMaintenanceBlocker(array, admin) {
  return !admin ? T('elevation.admin_only')
    : !(array?.parityDisks || []).length ? T('elastic.no_parity')
      : !array.enabled || !array.parityRunAvailable ? T('elastic.maintenance_not_ready')
        : (array.snapraid?.history || []).some((run) => run.outcome === 'running') ? T('elastic.run_running') : '';
}

// The card's Dyski row (n05): "3 danych + 1 Parity (SnapRAID) + 1 cache".
// Counted from the array's own member lists; the ZFS card's row uses the same
// data/cache phrases.
export function elasticDisksText(array) {
  const parity = (array.parityDisks || []).length;
  const cache = (array.cacheDisks || []).length;
  return [
    T('pools.disks_data', { n: (array.dataDisks || []).length }),
    ...(parity ? [T('pools.disks_parity', { n: parity })] : []),
    ...(cache ? [T('pools.disks_cache', { n: cache })] : []),
  ].join(' + ');
}

export function elasticState(array) {
  const labels = { active: T('elastic.active'), pending: T('elastic.pending'), creating: T('elastic.creating'), needs_attention: T('elastic.error'), error: T('elastic.error'), disabled: T('elastic.disabled'), unknown: T('elastic.unknown') };
  return { label: labels[array.state] || labels.unknown, tone: array.state === 'active' ? 'ok' : ['error', 'needs_attention'].includes(array.state) ? 'err' : 'warn' };
}

// The protection sentence AND the tone that sentence deserves, so the card on
// the Pools tab and the mini row on the node dashboard cannot disagree about
// how alarming the same state is. `unknown` is a WARNING, not an ok: an
// unmeasured protection window is not a confirmed one.
export function elasticProtection(array) {
  if (!(array.parityDisks || []).length) return { label: T('elastic.no_parity'), tone: 'warn' };
  const labels = { protected: T('elastic.protected'), window_open: T('elastic.window_open'), unprotected: T('elastic.unprotected'), unknown: T('elastic.unknown') };
  const tones = { protected: 'ok', window_open: 'warn', unprotected: 'err', unknown: 'warn' };
  const status = array.protection?.status;
  return { label: labels[status] || labels.unknown, tone: tones[status] || tones.unknown };
}

const protectionLabel = (array) => elasticProtection(array).label;

export function elasticCapacity(array) {
  const parity = array.parityDisks || [];
  const parityBytes = parity.every((d) => knownBytes(d.sizeBytes)) ? parity.reduce((n, d) => n + Number(d.sizeBytes), 0) : null;
  const measured = knownBytes(array.usableBytes) && knownBytes(array.usedBytes) && Number(array.usedBytes) <= Number(array.usableBytes);
  const free = measured ? Number(array.usableBytes) - Number(array.usedBytes) : null;
  const raw = measured && parityBytes != null ? Number(array.usableBytes) + parityBytes : null;
  return { measured, free, parityBytes, raw };
}

// Stable per-array skeleton (n05, Pools tab): the icon, the name, the
// "Elastic Array" chip and the "Szczegóły" button never move on their own.
// Everything that DOES — the state chip, the topology line, the capacity
// split, the legend figures, the mountpoint/last-sync/protection rows and the
// state-detail reason — is left as an empty slot here and written into this
// SAME markup afterwards by `paintElasticCard`, the same split
// `poolCardSkeletonHtml`/`paintPoolCard` use for a ZFS pool card (pools.js).
// Before this, `elasticCardHtml` baked used bytes, the last sync and the
// state straight into the returned string, so any write to the array (used
// bytes), a completed sync or a state change gave `patchKeyedList` a fresh
// string and rebuilt the whole card, "Szczegóły" button included (M2,
// critic-round2-wave1-2026-09-22.md).
export function elasticCardSkeletonHtml(array) {
  return `<div class="pool-card nas-elastic-card" data-array="${escapeAttr(array.name)}">
    <div class="pc-head">
      <div class="pc-ico">${sprite('cylinder')}</div>
      <div class="pc-meta"><span class="pc-name">${escapeHtml(array.name)}</span>
        <tf-chip dot data-f="state"></tf-chip>
        <span ${SLOT} data-slot="cache-waiting"></span>
        <tf-chip status="accent" label="Elastic Array"></tf-chip>
        <div class="pc-desc" data-f="desc"></div>
      </div>
      <div class="pc-actions">
        <tf-button variant="secondary" size="sm" icon="external-link" data-act="array-details">${escapeHtml(T('elastic.details'))}</tf-button>
        <tf-button variant="ghost" size="sm" icon="refresh" data-act="array-sync">${escapeHtml(T('elastic.sync_now'))}</tf-button>
      </div>
    </div>
    <div class="pc-body">
      <div>
        <div class="pc-cap"><span>${escapeHtml(T('elastic.capacity'))}</span><span class="v" data-f="cap-value"></span></div>
        <div class="split-bar" data-f="bar"><span class="data"></span><span class="free"></span><span class="parity"></span></div>
        <div class="legend-rows mt-sm">
          <div class="lr"><span class="sw data"></span>${escapeHtml(T('elastic.used'))}<span class="v" data-f="v-used"></span></div>
          <div class="lr"><span class="sw free"></span>${escapeHtml(T('elastic.free'))}<span class="v" data-f="v-free"></span></div>
          <div class="lr"><span class="sw parity"></span>${escapeHtml(T('elastic.parity'))}<span class="v" data-f="v-parity"></span></div>
        </div>
        <div ${SLOT} data-slot="unmeasured"></div>
      </div>
      <div class="stat-rows">${rowSkel(T('pools.row_disks'), 'disks')}${rowSkel(T('elastic.mountpoint'), 'mountpoint')}${rowSkel(T('elastic.last_sync'), 'last-sync')}${rowSkel(T('elastic.card_mover'), 'mover')}${rowSkel(T('elastic.protection'), 'protection')}</div>
    </div>
    <div ${SLOT} data-slot="reason"></div>
  </div>`;
}

// Writes everything that DOES move into a skeleton `patchKeyedList` just kept
// or just built, on the SAME element for as long as that array exists — an
// open `tf-menu` has no place on this card, but the "Szczegóły" button does,
// and it survives every one of these calls exactly like a ZFS card's does
// (`paintPoolCard`).
//
// `admin` and `syncBusy` (a sync request of this card is in flight) only
// decide whether "Sync teraz" is enabled; the reason it is not is its tooltip.
export function paintElasticCard(card, array, { admin = false, syncBusy = false } = {}) {
  if (!card) return;
  const state = elasticState(array);
  const c = elasticCapacity(array);
  const widths = c.raw > 0 ? [Number(array.usedBytes), c.free, c.parityBytes].map((n) => n / c.raw * 100) : null;

  const stateChip = field(card, 'state');
  setAttr(stateChip, 'status', state.tone);
  setAttr(stateChip, 'label', state.label);
  setText(field(card, 'desc'), T('elastic.topology', { data: (array.dataDisks || []).length, parity: (array.parityDisks || []).length, fs: String(array.filesystem || '').toUpperCase() }));

  // The same "used / usable (pct%)" line a ZFS card has; an unmeasured array
  // has no percentage to show, so it keeps the two dashes.
  setText(field(card, 'cap-value'), c.measured
    ? T('pools.capacity_value', { used: fmtBytes(array.usedBytes), usable: fmtBytes(array.usableBytes), pct: pct(array.usedBytes, array.usableBytes) })
    : `${fmtOptionalBytes(array.usedBytes)} / ${fmtOptionalBytes(array.usableBytes)}`);

  // Fixed 3 spans (data/free/parity) throughout, so the "measured" and
  // "unmeasured" states differ only by a class and by whether each span
  // carries a width — never by which nodes exist, which is what a keyed
  // patch needs to leave the card's own identity alone.
  const bar = field(card, 'bar');
  setClass(bar, 'nas-unmeasured', !widths);
  setAttr(bar, 'aria-label', widths ? T('elastic.capacity') : T('elastic.unmeasured'));
  const [dataBar, freeBar, parityBar] = bar.children;
  setAttr(dataBar, 'style', widths ? `width:${widths[0]}%` : null);
  setAttr(freeBar, 'style', widths ? `width:${widths[1]}%` : null);
  setAttr(parityBar, 'style', widths ? `width:${widths[2]}%` : null);

  setText(field(card, 'v-used'), fmtOptionalBytes(array.usedBytes));
  setText(field(card, 'v-free'), fmtOptionalBytes(c.free));
  setText(field(card, 'v-parity'), fmtOptionalBytes(c.parityBytes));

  slotEl(card.querySelector('[data-slot="unmeasured"]'), !widths, 'unmeasured', `<div class="hint">${escapeHtml(T('elastic.unmeasured'))}</div>`);

  setText(field(card, 'disks'), elasticDisksText(array));
  setText(field(card, 'mountpoint'), array.unionPath || '—');
  setText(field(card, 'last-sync'), fmtDate(array.protection?.protectedAsOf));
  // n05 M9: the mover as n11 states it (moving is automatic; a schedule is
  // only a window), and the bytes waiting on the cache outside parity as the
  // head's warning chip — the node's own figure, shown only when non-zero.
  setText(field(card, 'mover'), moverScheduleValue(array.mover || {}));
  setText(field(card, 'protection'), protectionLabel(array));
  const waiting = cacheWaitingBytes(array);
  const waitingChip = slotEl(card.querySelector('[data-slot="cache-waiting"]'), waiting != null, 'chip', '<tf-chip dot status="warn"></tf-chip>');
  if (waitingChip) setAttr(waitingChip, 'label', T('elastic.card_cache_waiting', { size: fmtOptionalBytes(waiting) }));

  const stateDetail = elasticStateDetail(array);
  const reason = slotEl(card.querySelector('[data-slot="reason"]'), Boolean(stateDetail), 'reason', '<div class="pc-reason"></div>');
  if (reason) {
    setText(reason, stateDetail);
    setAttr(reason, 'title', elasticStateTitle(array));
  }

  const syncBlocked = elasticMaintenanceBlocker(array, admin);
  const sync = card.querySelector('[data-act="array-sync"]');
  setAttr(sync, 'disabled', syncBusy || Boolean(syncBlocked));
  setAttr(sync, 'title', syncBlocked || T('elastic.sync_now'));
}

// ---------------------------------------------------------------------------
// Patch primitives of the detail pane
// ---------------------------------------------------------------------------

// A field of the pane by its `data-f` name. Names are unique per pane.
const field = (root, name) => root.querySelector(`[data-f="${name}"]`);

// A stat row whose VALUE is patched later; the label is fixed markup.
const rowSkel = (label, name) => `<div class="sr"><span class="k">${escapeHtml(label)}</span><span class="v" data-f="${name}"></span></div>`;

// The cadence pill is the control: clicking the cadence is how n11 reaches the
// dialog that sets it, which is where an admin looks for it first. Only the
// text inside it moves on a poll.
const pillRowSkel = (label, name, act, admin) => (admin
  ? `<div class="sr"><span class="k">${escapeHtml(label)}</span><span class="v"><button type="button" class="sched-pill" data-act="${escapeAttr(act)}" title="${escapeAttr(T('elastic.schedule_edit'))}">${sprite('clock')} <span data-f="${name}"></span></button></span></div>`
  : rowSkel(label, name));

// Keys that must stay unique although the wire gives no id: two runs with the
// same identity fields get `#1`, `#2`… in their list order, so a duplicate is
// never silently dropped by the keyed patch.
function uniqueKeys(items, keyOf) {
  const seen = new Map();
  return items.map((item) => {
    const base = keyOf(item);
    const n = seen.get(base) || 0;
    seen.set(base, n + 1);
    return n ? `${base}#${n}` : base;
  });
}

// ---------------------------------------------------------------------------
// Disk cells
// ---------------------------------------------------------------------------

// A cell is keyed by the member's SLOT and disk id — what the node's rows key
// the member by — and its markup is fixed per key. Everything that describes
// the disk's current condition (its name, the usage figure, the tri-states,
// the kind badge, the repair button) is written into that one cell, so a poll
// that moves a figure or a history row leaves the cell and its buttons as the
// same nodes. Only a member that joins or leaves the array adds or removes a
// cell.
const diskKey = (disk) => `disk:${disk.name}:${disk.diskId}`;

function diskSkeletonHtml(disk) {
  return `<div class="disk-cell" data-disk="${escapeAttr(disk.diskId)}" data-branch="${escapeAttr(disk.name)}">
    <span class="health-dot"></span>
    <div class="dc-main"><div class="dc-name"><span data-f="disk-name"></span></div>
      <div ${SLOT} data-slot="absent"></div>
      <div ${SLOT} data-slot="last-seen"></div>
      <div class="dc-sub"><span data-fig="disk-usage"></span> · <span data-f="disk-fs"></span></div>
      <div class="dc-sub" data-f="disk-state"></div>
    </div><span ${SLOT} data-slot="kind"></span><span ${SLOT} data-slot="fix"></span><tf-button variant="ghost" size="sm" icon="external-link" data-act="disk"></tf-button>
  </div>`;
}

// `repair` is the reason a repair is offered on THIS disk, or '' when it is
// not. It is rendered as its own button rather than folded into the drill-in,
// because a repair overwrites the disk and must never be one click away from
// "show me this disk".
//
// The cell is named by the disk's KERNEL name (n11: `sdg`, `nvme2n1`). The
// slot stays in `data-branch` because the repair finds the member by it; the
// by-uuid device and the branch mountpoint identify, they do not name, so
// they are the name's tooltip.
//
// The naming rule (`disks::ShownDiskName` on the node): a LIVE name when
// there is one; otherwise the member's part in the array, with the name it
// was LAST seen under on its own line, marked as such — never in the name's
// place. "brak dysku" is said ONLY when the helper says the device is absent
// (`devicePresent === false`): a member it found is not missing just because
// no name reached this cell, and the cell must not contradict its own
// "Obecny: tak" line.
function paintDiskCell(cell, disk, filesystem, repair = '') {
  if (!cell) return;
  const live = String(disk.diskName || '').trim();
  const lastKnown = String(disk.diskLastName || '').trim();
  const absent = !live && disk.devicePresent === false;
  const dot = cell.querySelector('.health-dot');
  const dotClass = `health-dot ${healthClass(disk.health)}`;
  if (dot.className !== dotClass) dot.className = dotClass;
  const nameEl = field(cell, 'disk-name');
  setText(nameEl, live || (absent ? T('elastic.disk_absent') : memberPart(disk)));
  setClass(nameEl, 'mono', Boolean(live));
  setClass(nameEl, 'num-err', absent);
  setAttr(nameEl, 'title', [disk.device, disk.mountpoint].filter(Boolean).join(' · '));
  const part = slotEl(cell.querySelector('[data-slot="absent"]'), absent, 'absent', '<div class="dc-sub"></div>');
  if (part) setText(part, memberPart(disk));
  const seen = slotEl(cell.querySelector('[data-slot="last-seen"]'), !live && Boolean(lastKnown), 'last-seen', '<div class="dc-sub" data-role="last-seen"></div>');
  if (seen) setText(seen, T('elastic.last_seen_as', { name: lastKnown }));
  setText(cell.querySelector('[data-fig="disk-usage"]'), `${fmtOptionalBytes(disk.usedBytes)} / ${fmtOptionalBytes(disk.sizeBytes)}`);
  setText(field(cell, 'disk-fs'), String(filesystem || '').toUpperCase());
  setText(field(cell, 'disk-state'), `${T('elastic.mounted')}: ${triState(disk.mounted)} · ${T('elastic.present')}: ${triState(disk.devicePresent)}`);
  // The kind is part of the badge's key, so a changed kind swaps the one
  // badge and nothing else.
  const kind = String(disk.kind || '');
  const [kindClass, kindLabel] = KIND_BADGE[kind] || [];
  slotEl(cell.querySelector('[data-slot="kind"]'), Boolean(kindLabel), `kind:${kind}`, `<span class="disk-kind ${escapeAttr(kindClass || '')}">${escapeHtml(kindLabel || '')}</span>`);
  const fix = slotEl(cell.querySelector('[data-slot="fix"]'), Boolean(repair), 'fix', `<tf-button variant="danger" size="sm" icon="shield" data-act="fix">${escapeHtml(T('elastic.repair'))}</tf-button>`);
  if (fix) setAttr(fix, 'title', repair);
  // An icon-only button: its accessible name is its title, and the inner
  // <button> the component builds is what assistive tech reads.
  const drill = cell.querySelector('[data-act="disk"]');
  const title = T('elastic.disk_details', { name: memberName(disk) });
  setAttr(drill, 'title', title);
  setAttr(drill.querySelector('button'), 'aria-label', title);
}

// One disk group's cells: keyed, then painted. `repairOf(disk)` answers the
// repair reason for that member ('' for none).
function paintDiskCells(host, disks, filesystemOf, repairOf = () => '') {
  patchKeyedList(host, disks.map((d) => ({ key: diskKey(d), html: diskSkeletonHtml(d) })));
  const cells = [...host.children];
  disks.forEach((d, i) => paintDiskCell(cells[i], d, filesystemOf(d), repairOf(d)));
}

/// The repair dialog. The retype is the DISK and not the array: what the
/// operation overwrites is that one disk, so the mistake worth making
/// impossible is repairing the wrong disk of the right array.
///
/// The loss line names the disk by that same kernel name, never by its
/// branch mountpoint (`…/data/d1` ends in the internal slot name).
///
/// The admin retypes the name they SEE on the cell, the kernel name — which
/// is why the repair is offered only on a member that has one. The request
/// still addresses the member by its slot (`disk`, echoed in `confirmDisk`,
/// which the node compares with it): the slot is what the node's rows key the
/// member by, and a kernel name can move between boots.
function openElasticFixDialog(screen, array, disk, evidence, onDone) {
  const shown = memberName(disk);
  const bodyHtml = `
    ${warningHtml('danger', T('elastic.repair_warning', { disk: shown, name: array.name }))}
    <ul class="loss-list">
      <li class="ll bad">${sprite('trash')}<span>${escapeHtml(T('elastic.repair_loses', { disk: shown }))}</span></li>
      <li class="ll">${sprite('shield')}<span>${escapeHtml(evidence)}</span></li>
    </ul>
    <div class="explain-box">${escapeHtml(T('elastic.repair_explain'))}</div>`;
  return openRetypeDialog({
    ...NAS_DIALOG,
    title: T('elastic.repair_title', { disk: shown, name: array.name }),
    icon: 'alert',
    name: shown,
    bodyHtml,
    retypeLabel: `${escapeHtml(T('elastic.repair_retype'))} <span class="mono num-err">${escapeHtml(shown)}</span>`,
    confirmLabel: T('elastic.repair_confirm', { disk: shown }),
    confirmIcon: 'shield',
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasElasticArrayFixRequest', {
        name: array.name, disk: disk.name, confirmDisk: disk.name, sudoPassword,
      }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('elastic.repair_title', { disk: shown, name: array.name }));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('elastic.repair_done', { disk: shown }));
      return true;
    },
  });
}

// DISK REPLACEMENT IS WITHDRAWN (round 4, owner's decision). There is no
// affordance for it here on purpose: the node refuses the request before it
// writes anything, and a button whose only possible answer is a refusal is
// worse than no button — the previous one wedged an array's whole maintenance
// surface with a single click. What an admin can do about a data disk that is
// gone travels with the refusal in
// `dispatch::tentanas::elastic_replace_disk`.

/// Adding one data disk. The free-disk list is fetched WHEN THE DIALOG OPENS
/// rather than polled with the screen: it is the one moment the answer has to
/// be current, and a stale list is how an admin picks a disk another array
/// took in the meantime.
async function openAddDataDiskDialog(screen, array, onDone) {
  let free = [];
  try {
    const res = await screen.nas('tentaNasElasticCapabilitiesRequest');
    if (!Array.isArray(res?.freeDisks)) throw new Error(T('elastic.bad_response'));
    free = res.freeDisks.filter((d) => d.health !== 'critical');
  } catch (error) {
    toast(T('elastic.add_disk_failed', { error: errMessage(error) }), 'error');
    return null;
  }
  // The parity ceiling is the node's rule and the ONLY warning that arrives in
  // time: snapraid accepts a data disk larger than its parity and refuses only
  // months later, once the data has outgrown it (MEASURED 2026-09-06,
  // snapraid 14.7). So an oversized disk stays visible with the reason instead
  // of being silently dropped from the list.
  // `knownBytes`, not `Number(...) || 0`: a parity disk this node has not
  // measured reports `null`, and reading that as zero made EVERY candidate
  // "większy niż parity (0 B)" — one unmeasured parity disk disabled the whole
  // list. An unmeasured parity gives no floor at all, so the node's own check
  // is the one that decides.
  const parityBytes = (array.parityDisks || []).filter((p) => knownBytes(p.sizeBytes));
  const parityFloor = parityBytes.length === (array.parityDisks || []).length && parityBytes.length
    ? Math.min(...parityBytes.map((p) => Number(p.sizeBytes)))
    : null;
  const reasonFor = (disk) => parityFloor != null && knownBytes(disk.sizeBytes)
    && Number(disk.sizeBytes) > parityFloor
    ? T('elastic.add_disk_over_parity', { parity: fmtOptionalBytes(parityFloor) })
    : '';
  if (!free.length) {
    toast(T('elastic.add_disk_none'), 'error');
    return null;
  }
  let picked = '';
  const cells = free.map((disk) => {
    const reason = reasonFor(disk);
    return `<div class="disk-cell ${reason ? 'disabled' : ''}" data-disk="${escapeAttr(disk.diskId)}" title="${escapeAttr(reason)}">
      <div class="dc-main"><div class="dc-name mono">${escapeHtml(disk.name)}</div>
      <div class="dc-sub">${escapeHtml(fmtOptionalBytes(disk.sizeBytes))} · ${escapeHtml(disk.serial || '—')}${reason ? ` · ${escapeHtml(reason)}` : ''}</div></div></div>`;
  }).join('');
  const bodyHtml = `
    ${warningHtml('danger', T('elastic.add_disk_warning', { name: array.name }))}
    <p class="wizard-section-sub">${escapeHtml(T('elastic.add_disk_sub'))}</p>
    <div class="disk-cells" id="nas-add-disk">${cells}</div>
    <div class="explain-box">${escapeHtml(T('elastic.add_disk_explain'))}</div>`;
  return openRetypeDialog({
    ...NAS_DIALOG,
    title: T('elastic.add_disk_title', { name: array.name }),
    icon: 'plus',
    name: array.name,
    bodyHtml,
    retypeLabel: `${escapeHtml(T('elastic.add_disk_retype'))} <span class="mono num-err">${escapeHtml(array.name)}</span>`,
    confirmLabel: T('elastic.add_disk_confirm'),
    confirmIcon: 'plus',
    width: 620,
    wire: (win) => {
      win.querySelectorAll('#nas-add-disk .disk-cell').forEach((cell) => cell.addEventListener('click', () => {
        if (cell.classList.contains('disabled')) return;
        picked = cell.dataset.disk;
        // Only the two cells whose selection actually changed are touched —
        // the list is not rebuilt, so nothing the admin is reading moves.
        win.querySelectorAll('#nas-add-disk .disk-cell.checked').forEach((other) => other.classList.remove('checked'));
        cell.classList.add('checked');
      }));
    },
    onConfirm: async () => {
      if (!picked) throw new Error(T('elastic.add_disk_pick'));
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasElasticArrayAddDiskRequest', {
        name: array.name, diskId: picked, confirmName: array.name, sudoPassword,
      }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('elastic.add_disk_title', { name: array.name }));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('elastic.add_disk_done'));
      return true;
    },
  });
}

/// Finishing an add that stopped part-way (F3). There is no picker: the only
/// disk this array admits is the one the add pinned, down to the filesystem
/// UUID its mkfs was given, and the node resumes it from the step it stopped
/// in. The retype is the array's name, as for the add itself.
function openResumeAddDiskDialog(screen, array, onDone) {
  const pending = pendingAdd(array);
  if (!pending) return null;
  const disk = pendingAddName(pending);
  const bodyHtml = `
    ${warningHtml('info', `${T('elastic.add_disk_pending', { disk })} ${pendingStepText(pending)}`)}
    <div class="explain-box">${escapeHtml(T('elastic.add_disk_resume_explain'))}</div>`;
  return openRetypeDialog({
    title: T('elastic.add_disk_resume_title', { disk, name: array.name }),
    icon: 'play',
    name: array.name,
    bodyHtml,
    retypeLabel: `${escapeHtml(T('elastic.add_disk_retype'))} <span class="mono num-err">${escapeHtml(array.name)}</span>`,
    confirmLabel: T('elastic.add_disk_resume_confirm'),
    confirmIcon: 'play',
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasElasticArrayAddDiskRequest', {
        name: array.name, diskId: pending.diskId, confirmName: array.name, sudoPassword,
      }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('elastic.add_disk_resume_title', { disk, name: array.name }));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('elastic.add_disk_resume_done'));
      return true;
    },
  });
}

/// Undoing an add that stopped before its disk joined the share (D3 = b).
/// Offered only while the node says it is possible (`undoPossible`, the
/// helper's own gate: its journal proves the add never tried to join the
/// share, and a live read of the union can only add a refusal): once the
/// disk may have served a file, the add can only be finished.
function openUndoAddDiskDialog(screen, array, onDone) {
  const pending = pendingAdd(array);
  if (!pending?.undoPossible) return null;
  const disk = pendingAddName(pending);
  const bodyHtml = `
    ${warningHtml('danger', T('elastic.add_disk_undo_warning', { disk, name: array.name }))}
    <div class="explain-box">${escapeHtml(T('elastic.add_disk_undo_explain'))}</div>`;
  return openRetypeDialog({
    title: T('elastic.add_disk_undo_title', { disk, name: array.name }),
    icon: 'rotate',
    name: array.name,
    bodyHtml,
    retypeLabel: `${escapeHtml(T('elastic.add_disk_retype'))} <span class="mono num-err">${escapeHtml(array.name)}</span>`,
    confirmLabel: T('elastic.add_disk_undo_confirm'),
    confirmIcon: 'rotate',
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasElasticArrayAddDiskAbortRequest', {
        name: array.name, diskId: pending.diskId, confirmName: array.name, sudoPassword,
      }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('elastic.add_disk_undo_title', { disk, name: array.name }));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('elastic.add_disk_undo_done'));
      return true;
    },
  });
}

// The mergerfs create policy the node mounts the union with (`mfs`, …), in
// the reader's words — the code is mergerfs' own shorthand. A policy this
// build has no words for is shown as the node sent it: truthful, if terse.
export function createPolicyLabel(policy) {
  const code = String(policy || '');
  if (!code) return '';
  const key = 'elastic.create_policy_' + code;
  const words = T(key);
  return words === 'tentanas.' + key ? code : words;
}

/// A Sync over an unrepaired Scrub or Repair fault (F1). The confirm names
/// the cost before anything is sent, and it is the ONLY place that sends
/// `acknowledgeParityFault`: the id of the very fault it showed
/// (`syncFaultId`), which the node and its helper compare with the fault
/// the array carries when the Sync runs. Measured on rig11 (M3, design
/// repository reviews/artifacts/elastic-measurements-2026-09-24): marked
/// blocks and unchanged files the Scrub could not read stay repairable across
/// such a Sync; what it finalises is the state of files deleted or changed
/// since the previous Sync, damaged ones included. Shared by the detail pane and the n05
/// card, so the two "Sync teraz" buttons cannot disagree.
export function openSyncOverFaultDialog(screen, array, onDone) {
  // One confirm per array at a time: a second click does not open a second
  // window that could send the Sync twice.
  const open = [...document.querySelectorAll('tf-window[data-sync-fault]')].find((w) => w.dataset.syncFault === array.name);
  if (open) return open;
  // The fault the admin is confirming, by the id of the run that recorded
  // it — an internal key, never shown. A newer fault by the time the request
  // runs is refused by the node, not silently acknowledged.
  const fault = String(array.syncFaultId || '');
  const bodyHtml = `
    ${warningHtml('danger', T('elastic.sync_fault_warning', { name: array.name }))}
    <ul class="loss-list">
      <li class="ll bad">${sprite('trash')}<span>${escapeHtml(T('elastic.sync_fault_loses'))}</span></li>
      <li class="ll">${sprite('shield')}<span>${escapeHtml(T('elastic.sync_fault_keeps'))}</span></li>
    </ul>
    <div class="explain-box">${escapeHtml(T('elastic.sync_fault_explain'))}</div>`;
  const win = openRetypeDialog({
    title: T('elastic.sync_fault_title', { name: array.name }),
    icon: 'alert',
    name: array.name,
    bodyHtml,
    retypeLabel: `${escapeHtml(T('elastic.sync_fault_retype'))} <span class="mono num-err">${escapeHtml(array.name)}</span>`,
    confirmLabel: T('elastic.sync_fault_confirm'),
    confirmIcon: 'refresh',
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasElasticArraySyncRequest', {
        name: array.name, acknowledgeParityFault: fault, sudoPassword,
      }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('elastic.sync_fault_title', { name: array.name }));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('elastic.maintenance_accepted'));
      return true;
    },
  });
  win.dataset.syncFault = array.name;
  return win;
}

/// The danger zone. It says plainly what dissolving does NOT do, because that
/// is the whole shape of the operation: nothing is formatted, so the disks keep
/// their filesystems and their files and the array import takes the array back.
function openElasticDestroyDialog(screen, array, onDone) {
  const disks = [...(array.dataDisks || []), ...(array.cacheDisks || [])];
  const bodyHtml = `
    ${warningHtml('danger', T('elastic.dissolve_warning', { name: array.name }))}
    <ul class="loss-list">
      <li class="ll bad">${sprite('trash')}<span>${escapeHtml(T('elastic.dissolve_loses', { path: array.unionPath }))}</span></li>
      <li class="ll">${sprite('shield')}<span>${escapeHtml(T('elastic.dissolve_keeps', { n: disks.length, disks: disks.map(memberName).join(', ') || '—' }))}</span></li>
      <li class="ll">${sprite('info')}<span>${escapeHtml(T('elastic.dissolve_reimport'))}</span></li>
    </ul>
    <div class="explain-box">${escapeHtml(T('elastic.dissolve_explain'))}</div>`;
  return openRetypeDialog({
    ...NAS_DIALOG,
    title: T('elastic.dissolve_title', { name: array.name }),
    icon: 'alert',
    name: array.name,
    bodyHtml,
    retypeLabel: `${escapeHtml(T('elastic.dissolve_retype'))} <span class="mono num-err">${escapeHtml(array.name)}</span>`,
    confirmLabel: T('elastic.dissolve_confirm', { name: array.name }),
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasElasticArrayDestroyRequest', {
        name: array.name, confirmName: array.name, sudoPassword,
      }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('elastic.dissolve_title', { name: array.name }));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('elastic.dissolve_done', { name: array.name }));
      return true;
    },
  });
}

const RUN_OUTCOMES = { running: 'run_running', ok: 'run_ok', partial: 'run_partial', interrupted: 'run_interrupted', nothing_repaired: 'run_nothing_repaired', failed: 'run_failed', needs_attention: 'error', refused: 'run_refused' };
const RUN_REFUSALS = {
  no_parity: 'no_parity', precondition_failed: 'refused_precondition', unsynced_changes: 'refused_dirty', empty_parity: 'refused_empty',
  // The helper's admission gate (helper 0.14.0): nothing was changed.
  operation_pending: 'refused_operation_pending', attention_other: 'refused_attention_other',
  fault_unacknowledged: 'refused_fault_unacknowledged', attention_add_disk: 'refused_attention_add_disk',
};
const runKey = (run) => JSON.stringify([run.operationId, run.jobId, run.startedAt, run.kind]);

// A run's row is keyed by its identity (operation, job, start, kind), so its
// markup is fixed per key: the kind label and the job link never change for
// the same run. What a running run changes as it ends — the outcome chip, the
// finish time, the counters — is written into the row, which keeps its
// <details> (and whether the admin opened it) across polls.
function runSkeletonHtml(run, key) {
  const label = run.kind === 'sync' ? 'Sync' : run.kind === 'scrub' ? 'Scrub' : run.kind === 'fix' ? T('elastic.run_fix') : run.kind;
  return `<li><details data-run="${escapeAttr(key)}"><summary><strong>${escapeHtml(label)}</strong><span class="hint" data-f="run-when"></span><tf-chip></tf-chip></summary>
      <div class="stat-rows">${rowSkel(T('elastic.run_started'), 'run-started')}${rowSkel(T('elastic.run_finished'), 'run-finished')}${rowSkel(T('elastic.run_blocks'), 'run-blocks')}${rowSkel(T('elastic.run_errors'), 'run-errors')}${rowSkel(T('elastic.run_exit'), 'run-exit')}</div>
      <div ${SLOT} data-slot="run-detail"></div></details>${run.jobId ? `<tf-button variant="ghost" size="sm" data-act="history-job" data-job="${escapeAttr(run.jobId)}">${escapeHtml(T('elastic.history_job'))}</tf-button>` : ''}</li>`;
}

function paintRun(li, run) {
  const chip = li.querySelector('summary tf-chip');
  setAttr(chip, 'status', run.outcome === 'ok' ? 'ok' : run.outcome === 'failed' || run.outcome === 'needs_attention' ? 'err' : 'warn');
  setAttr(chip, 'label', T(`elastic.${RUN_OUTCOMES[run.outcome] || 'unknown'}`));
  setText(field(li, 'run-when'), fmtDate(run.finishedAt || run.startedAt));
  setText(field(li, 'run-started'), fmtDate(run.startedAt));
  setText(field(li, 'run-finished'), fmtDate(run.finishedAt));
  setText(field(li, 'run-blocks'), `${run.checkedBlocks ?? '—'} / ${run.totalBlocks ?? '—'}`);
  setText(field(li, 'run-errors'), `${run.errorsFile ?? '—'} / ${run.errorsIo ?? '—'} / ${run.errorsData ?? '—'}`);
  setText(field(li, 'run-exit'), run.exitCode ?? '—');
  const detail = run.outcome === 'refused' && RUN_REFUSALS[run.detail] ? T(`elastic.${RUN_REFUSALS[run.detail]}`) : run.detail;
  const hint = slotEl(li.querySelector('[data-slot="run-detail"]'), Boolean(detail), 'detail', '<div class="hint"></div>');
  if (hint) setText(hint, detail);
}

// `expanded` carries the open runs across a row that IS rebuilt — the pane
// coming back after a failed read — so a fresh row opens the way the admin
// left it. A row that survives a poll keeps its own <details> state.
function paintHistory(section, history, expanded) {
  const list = slotEl(section.querySelector('[data-slot="history-list"]'), history.length > 0, 'list', '<ol></ol>');
  slotEl(section.querySelector('[data-slot="history-empty"]'), history.length === 0, 'empty', `<div class="hint mt-sm">${escapeHtml(T('elastic.history_empty'))}</div>`);
  if (!list) return;
  const keys = uniqueKeys(history, runKey);
  patchKeyedList(list, history.map((run, i) => ({ key: keys[i], html: runSkeletonHtml(run, keys[i]) })));
  const rows = [...list.children];
  history.forEach((run, i) => {
    const li = rows[i];
    if (!li.__tfRun) {
      li.__tfRun = true;
      li.querySelector('details').open = expanded.has(keys[i]);
    }
    paintRun(li, run);
  });
}

const moverMoved = (run) => !run ? '—' : `${fmtOptionalBytes(run.movedBytes)} · ${Number(run.movedFiles) || 0}`;

// `countsKnown: false` means the walk never finished: such a run knows what it
// MOVED and NOT what it left behind. A `0` here would say "nothing was skipped",
// which is the one thing that run cannot say — so it reads "nie zmierzono".
const moverSkipped = (run) => !run ? '—'
  : run.countsKnown ? `${fmtOptionalBytes(run.skippedBytes)} · ${Number(run.skippedFiles) || 0}`
    : T('elastic.mover_counts_unmeasured');

// A partial sync is not a failure: files changed while it ran, and the next
// sync covers them.
const moverSync = (run) => !run.coupledSync ? T('elastic.mover_sync_none')
  : run.coupledSync.outcome === 'ok' ? T('elastic.mover_sync_ok')
    : run.coupledSync.outcome === 'partial' ? T('elastic.mover_sync_partial') : T('elastic.mover_sync_failed');

const moverLastRun = (run) => !run ? '—'
  : `${fmtDate(run.finishedAt || run.startedAt)} · ${fmtOptionalBytes(run.movedBytes)} → ${moverSync(run)}`;

// The age rule is a duration and the cache rule a FILL level, while the setting
// is the minimum FREE percentage — so the sentence n11 shows is its complement.
// `null` means one of the two halves is missing, and nothing invents it: both
// the rules row and the explanation above it drop the numbers together.
const moverRuleParams = (m) => {
  const age = Number(m.minAgeSecs);
  const free = Number(m.cacheMinFreePct);
  if (m.minAgeSecs == null || m.cacheMinFreePct == null || !Number.isFinite(age) || !Number.isFinite(free)) return null;
  return { age: fmtDuration(age), pct: 100 - free };
};

// Absent settings render as `—`.
// With nothing configured these numbers are the built-in defaults a run falls
// back on — real, but nobody's decision. They are shown (a manual run WILL
// apply them) and labelled as defaults, rather than presented as settings.
const moverRulesValue = (m) => {
  const params = moverRuleParams(m);
  if (!params) return '—';
  return m.configured ? T('elastic.mover_rules_value', params) : T('elastic.mover_rules_default', params);
};

// "Mover" is a name, not a description, so the panel says what the process
// does before it says anything about its state — with THIS array's own
// thresholds, so the sentence and the rules row below can never disagree.
const moverExplain = (m) => {
  const params = moverRuleParams(m);
  return params ? T('elastic.mover_explain', params) : T('elastic.mover_explain_no_rules');
};

// The schedule is not a cadence the mover needs — moving is automatic — but an
// optional WINDOW that restricts it. Saved-but-off restricts nothing, and says
// so, because reading like a live window would promise the disks rest.
const moverScheduleValue = (m) => (!m.schedule ? T('elastic.mover_window_none')
  : m.enabled ? T('elastic.mover_window_only', { when: fmtSchedule(m.schedule) })
    : T('elastic.mover_window_off', { when: fmtSchedule(m.schedule) }));

// A cadence and its switch are two facts. A schedule that is saved but off
// says so, because rendering it like a live one would promise a safety net
// that is not running.
const cadenceValue = (schedule, enabled) => (!schedule ? T('elastic.mover_schedule_none')
  : enabled ? fmtSchedule(schedule) : `${fmtSchedule(schedule)} · ${T('schedule.off')}`);

// Moving files off the cache is automatic, so nothing here is something an
// admin has to operate: the rules, the optional window, the manual run and the
// history live in a collapsed section. The main view carries only the one fact
// about the DATA (`cachePendingHtml`). The section is built once per pane and
// only written into, so a run starting or finishing never closes it on the
// admin who opened it.
function moverSkeletonHtml(admin) {
  return `<details class="section-card nas-mover" data-section="mover"><summary class="section-card-head"><div class="title">${sprite('transform')} ${escapeHtml(T('elastic.mover'))}</div></summary>
    <div class="actions mb-sm">
    ${admin ? `<tf-button variant="ghost" size="sm" icon="edit" data-act="mover-schedule">${escapeHtml(T('elastic.mover_settings'))}</tf-button>` : ''}
    <tf-button variant="secondary" size="sm" icon="play" data-act="mover">${escapeHtml(T('elastic.mover_run_now'))}</tf-button></div>
    <div class="explain-box mb-sm nas-mover-explain" data-f="mover-explain"></div>
    <div ${SLOT} data-slot="mover-reason"></div>
    <div ${SLOT} data-slot="mover-restricted"></div>
    <div class="stat-rows">${pillRowSkel(T('elastic.mover_schedule'), 'mover-window', 'mover-schedule', admin)}${rowSkel(T('elastic.mover_rules'), 'mover-rules')}${row(T('elastic.mover_open_files'), T('elastic.mover_open_files_skipped'))}${rowSkel(T('elastic.mover_last_run'), 'mover-last')}${rowSkel(T('elastic.mover_moved'), 'mover-moved')}${rowSkel(T('elastic.mover_skipped'), 'mover-skipped')}</div>
    <div class="mover-hist">${escapeHtml(T('elastic.mover_history'))}: <span data-part="mover-runs" ${SLOT}></span></div>
    <div class="explain-box mt-md nas-mover-parity-note" data-f="mover-parity-note"></div>
    <div class="hint mt-sm">${escapeHtml(T('elastic.mover_name_note'))}</div>
  </details>`;
}

function paintMover(section, array, disabled, reason) {
  const m = array.mover || {};
  const last = m.lastRun || null;
  const history = m.history || [];
  setAttr(section.querySelector('[data-act="mover"]'), 'disabled', disabled);
  setText(field(section, 'mover-explain'), moverExplain(m));
  const hint = slotEl(section.querySelector('[data-slot="mover-reason"]'), Boolean(reason), 'reason', '<div class="hint mb-sm"></div>');
  if (hint) setText(hint, reason);
  slotEl(section.querySelector('[data-slot="mover-restricted"]'), Boolean(m.enabled && m.schedule), 'restricted', `<div class="hint mb-sm">${escapeHtml(T('elastic.mover_restricted'))}</div>`);
  setText(field(section, 'mover-window'), moverScheduleValue(m));
  setText(field(section, 'mover-rules'), moverRulesValue(m));
  setText(field(section, 'mover-last'), moverLastRun(last));
  setText(field(section, 'mover-moved'), moverMoved(last));
  setText(field(section, 'mover-skipped'), moverSkipped(last));
  // One pill per run, keyed by the run: a new run adds its pill at the front
  // and every older pill stays the node it was.
  const keys = uniqueKeys(history, (run) => `run:${run.startedAt || ''}:${run.finishedAt || ''}`);
  patchKeyedList(section.querySelector('[data-part="mover-runs"]'), history.length
    ? history.map((run, i) => ({ key: keys[i], html: `<span>${escapeHtml(fmtDate(run.finishedAt || run.startedAt))} · ${escapeHtml(fmtOptionalBytes(run.movedBytes))}</span>` }))
    : [{ key: 'empty', html: `<span>${escapeHtml(T('elastic.mover_history_empty'))}</span>` }]);
  setText(field(section, 'mover-parity-note'), m.coupledSync === false ? T('elastic.mover_coupled_off') : T('elastic.mover_coupled_warning'));
}

// The one line the main view says about moving: how much data sits on the cache
// outside parity. A figure, never a fault — a non-zero value is the normal state
// of an array with a cache, and the stuck alert is what reports a problem. The
// number itself is written by the paint, so a changing byte count changes one
// text node.
const cachePendingHtml = () => `<div class="stat-rows nas-cache-pending"><div class="sr"><span class="k">${escapeHtml(T('elastic.cache_pending'))}</span><span class="v" data-fig="cache-pending"></span></div></div>`;

// §5.3's three per-folder answers, in the order the mockup lists them. `yes`
// is the DEFAULT and is spelled by the absence of a stored row on the node, so
// picking it is how a folder is returned to the default rather than a third
// value stored beside it.
const CACHE_POLICIES = ['yes', 'no', 'only'];
const cachePolicyLabel = (policy) => T(`elastic.cache_policy_${CACHE_POLICIES.includes(policy) ? policy : 'yes'}`);
const cachePolicySub = (policy) => T(`elastic.cache_policy_${CACHE_POLICIES.includes(policy) ? policy : 'yes'}_sub`);

/**
 * One folder's cache policy.
 *
 * The consequence of `only` is stated HERE, in the window where the choice is
 * made, and not only in the panel's explanation: the pin keeps that folder's
 * bytes on the cache, SnapRAID never covers the cache, and so those files stay
 * outside parity for as long as the pin lasts. An admin who has not read the
 * panel must still meet that sentence before saving it.
 */
export function openFolderCacheDialog(screen, array, folder, onDone) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal nas-folder-cache';
  win.setAttribute('title', T('elastic.folder_cache_title', { folder: folder.name }));
  win.setAttribute('icon', 'folder');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '520');
  win.setAttribute('min-width', '420');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="stat-rows">${row(T('elastic.folders_col_name'), folder.path || folder.name)}</div>
      <div><tf-select id="nas-folder-policy" label="${escapeAttr(T('elastic.folder_cache_label'))}"></tf-select>
        <div class="hint" id="nas-folder-policy-sub"></div></div>
      <div id="nas-folder-policy-warn" hidden>${warningHtml('danger', T('elastic.folder_pinned_warning', { folder: folder.name }))}</div>
      <div class="num-err" id="nas-folder-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="save" data-action="confirm">${escapeHtml(T('schedule.save'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const select = win.querySelector('#nas-folder-policy');
  const sub = win.querySelector('#nas-folder-policy-sub');
  const warn = win.querySelector('#nas-folder-policy-warn');
  // An unrecognised stored value falls back to the default rather than being
  // offered back as a fourth option nobody wrote.
  const current = CACHE_POLICIES.includes(folder.cachePolicy) ? folder.cachePolicy : 'yes';
  const paint = (policy) => {
    sub.textContent = cachePolicySub(policy);
    warn.hidden = policy !== 'only';
  };
  select.setOptions(CACHE_POLICIES.map((p) => ({ value: p, label: cachePolicyLabel(p) })), current);
  paint(current);
  select.addEventListener('change', (e) => paint(e.detail?.value || select.value));
  let busy = false;
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    busy = true;
    const btn = win.querySelector('[data-action="confirm"]');
    btn.setAttribute('disabled', '');
    try {
      await screen.nas('tentaNasElasticFolderCacheSetRequest', {
        name: array.name,
        folder: folder.name,
        cachePolicy: select.value,
      });
      toast(T('elastic.folder_cache_saved'), 'success');
      win.close(true);
      if (onDone) onDone();
    } catch (err) {
      busy = false;
      btn.removeAttribute('disabled');
      const errEl = win.querySelector('#nas-folder-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  return win;
}

// n11's "Foldery" table. The Cache cell is the control, the same way the
// cadence pill is the way into the schedule dialog one card down.
//
// `foldersKnown === false` is UNKNOWN and never "this array has no folders":
// §3.4 forbids fstab, so before the branches are mounted `/mnt/<array>` is an
// ordinary empty directory and the node's `read_dir` of it says nothing. The
// table therefore has THREE states, not two — a read list, an unread one, and
// an unread one that still carries the folders somebody pinned.
function foldersSkeletonHtml(admin) {
  return `<div class="section-card nas-folders"><div class="section-card-head"><div class="title">${sprite('folder')} ${escapeHtml(T('elastic.folders'))}</div><span class="hint">${escapeHtml(T('elastic.folders_hint'))}</span></div>
    <div ${SLOT} data-slot="folder-table"></div>
    <div ${SLOT} data-slot="folders-none"></div>
    <div ${SLOT} data-slot="folders-unknown"></div>
    <div ${SLOT} data-slot="folders-pinned"></div>
    <div class="explain-box mt-md">${escapeHtml(T('elastic.folders_explain'))}</div>
    ${admin ? '' : `<div class="hint mt-sm">${escapeHtml(T('elevation.admin_only'))}</div>`}
  </div>`;
}

// A row is keyed by the folder's name; what the node may change about it —
// its path, its share, its policy — is written into the row, so a policy
// saved in the dialog updates the pill's text instead of replacing the row.
function folderSkeletonHtml(folder, admin) {
  const cell = admin
    ? `<button type="button" class="sched-pill" data-act="folder-cache" data-folder="${escapeAttr(folder.name)}" title="${escapeAttr(T('elastic.folder_cache_edit'))}">${sprite('folder')} <span data-f="folder-policy"></span></button>`
    : '<span data-f="folder-policy"></span>';
  return `<div class="fr" data-folder="${escapeAttr(folder.name)}">
    <span class="fr-name"><span class="mono">${escapeHtml(folder.name)}</span><span class="fr-sub mono" data-f="folder-path"></span></span>
    <span class="fr-used num" data-f="folder-used"></span>
    <span class="fr-share" data-f="folder-share"></span>
    <span class="fr-cache">${cell}<span ${SLOT} data-slot="folder-pinned"></span></span>
  </div>`;
}

// Why a folder has no size, in the reader's language. The node measures
// folders by walking the disks in the background, hours apart, so "not yet"
// and "too many files to count in time" are ordinary answers, not faults.
const FOLDER_USAGE_WORDS = new Map([
  ['folder_usage_pending', () => T('elastic.folder_usage.pending')],
  ['folder_usage_over_budget', (p) => T('elastic.folder_usage.over_budget', { entries: Number(p.entries || 0).toLocaleString(I18n.getLanguage()), minutes: p.minutes || '' })],
  ['folder_usage_unreadable', () => T('elastic.folder_usage.unreadable')],
  ['folder_usage_not_mounted', () => T('elastic.folder_usage.not_mounted')],
  ['folder_usage_failed', () => T('elastic.folder_usage.failed')],
  ['folder_usage_name_refused', () => T('elastic.folder_usage.name_refused')],
]);

/**
 * One folder's "Użycie" cell: the measured bytes with their age as the
 * tooltip, or "—" with the reason. A folder with no figure and no reason
 * (an older node) says only that it was not measured.
 */
export function folderUsageCell(folder) {
  const bytes = folder?.usedBytes;
  if (bytes !== null && bytes !== undefined && Number.isFinite(Number(bytes))) {
    return {
      text: fmtBytes(Number(bytes)),
      title: folder.usedMeasuredAt ? T('elastic.folder_usage.measured', { ago: fmtAgo(folder.usedMeasuredAt) }) : '',
    };
  }
  return {
    text: '—',
    title: wordReasons(folder?.usedReasons, FOLDER_USAGE_WORDS) || T('elastic.folder_usage.unknown'),
  };
}

function paintFolders(card, array, admin) {
  const folders = array.folders || [];
  const known = array.foldersKnown === true;
  const table = slotEl(card.querySelector('[data-slot="folder-table"]'), folders.length > 0, 'table', '<div class="nas-folder-rows"></div>');
  if (table) {
    patchKeyedList(table, [
      { key: 'head', html: `<div class="fr fr-head"><span>${escapeHtml(T('elastic.folders_col_name'))}</span><span>${escapeHtml(T('elastic.folders_col_used'))}</span><span>${escapeHtml(T('elastic.folders_col_share'))}</span><span>${escapeHtml(T('elastic.folders_col_cache'))}</span></div>` },
      ...folders.map((folder) => ({ key: `folder:${folder.name}`, html: folderSkeletonHtml(folder, admin) })),
    ]);
    const rows = [...table.children].slice(1);
    folders.forEach((folder, i) => {
      const r = rows[i];
      setText(field(r, 'folder-path'), folder.path || '');
      const used = folderUsageCell(folder);
      const usedEl = field(r, 'folder-used');
      setText(usedEl, used.text);
      setAttr(usedEl, 'title', used.title || null);
      setText(field(r, 'folder-share'), folder.shareLabel || T('elastic.folders_share_none'));
      setText(field(r, 'folder-policy'), cachePolicyLabel(folder.cachePolicy));
      const chip = slotEl(r.querySelector('[data-slot="folder-pinned"]'), folder.cachePolicy === 'only', 'pinned', '<tf-chip status="warn"></tf-chip>');
      if (chip) setAttr(chip, 'label', T('elastic.folder_pinned_badge'));
    });
  }
  slotEl(card.querySelector('[data-slot="folders-none"]'), known && !folders.length, 'none', `<div class="hint mt-sm">${escapeHtml(T('elastic.folders_none'))}</div>`);
  const unknown = slotEl(card.querySelector('[data-slot="folders-unknown"]'), !known, 'unknown', '<div class="hint mt-sm"></div>');
  if (unknown) setText(unknown, T(folders.length ? 'elastic.folders_unknown_partial' : 'elastic.folders_unknown'));
  slotEl(card.querySelector('[data-slot="folders-pinned"]'), folders.some((f) => f.cachePolicy === 'only'), 'pinned', `<div class="wizard-warning danger nas-folders-pinned">${sprite('alert')}<div>${escapeHtml(T('elastic.folders_pinned_note'))}</div></div>`);
}

// The mockup's four choices (n15). 0 is "no age limit" and is a real setting,
// not an absent one — it means every file is old enough to move.
const MOVER_AGE_OPTIONS = [0, 1800, 7200, 86400];
const MOVER_FREE_OPTIONS = [10, 20, 30];
// The window may be as fine as a quarter of an hour: an admin who restricts
// moving to, say, every 6 h still wants the cache drained several times a day.
const MOVER_EVERY = ['15m', '30m', '1h', '6h', 'daily'];
// A window is for keeping the data disks quiet in working hours, so the one an
// admin starts from is the night.
const MOVER_SCHEDULE_DEFAULT = { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 };

/**
 * n15's mover dialog: the optional window AND the rules in one window, because
 * the mockup is one form. Sends both in one request, so an admin who changes
 * the age and the window together cannot end up with half of it saved. The
 * switch turns the window's RESTRICTION on; off, moving stays automatic.
 */
export function openMoverScheduleEditor(screen, array, onDone) {
  const m = array.mover || {};
  const schedule = m.schedule || MOVER_SCHEDULE_DEFAULT;
  const win = document.createElement('tf-window');
  win.className = 'nas-modal nas-mover-schedule';
  win.setAttribute('title', T('elastic.mover_schedule_title', { name: array.name }));
  win.setAttribute('icon', 'transform');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '560');
  win.setAttribute('min-width', '460');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('elastic.mover_window_label'))}</span><span class="tc-sub">${escapeHtml(T('elastic.mover_window_sub'))}</span></div>
        <tf-toggle id="nas-mover-enabled" ${m.enabled ? 'checked' : ''}></tf-toggle>
      </div>
      ${scheduleFieldsHtml('nas-mover', schedule, { allowed: MOVER_EVERY })}
      <div class="form-grid-2">
        <div><tf-select id="nas-mover-age" label="${escapeAttr(T('elastic.mover_min_age'))}"></tf-select><div class="hint">${escapeHtml(T('elastic.mover_min_age_hint'))}</div></div>
        <div><tf-select id="nas-mover-free" label="${escapeAttr(T('elastic.mover_min_free'))}"></tf-select><div class="hint">${escapeHtml(T('elastic.mover_min_free_hint'))}</div></div>
      </div>
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('elastic.mover_coupled_label'))}</span><span class="tc-sub">${escapeHtml(T('elastic.mover_coupled_sub'))}</span></div>
        <tf-toggle id="nas-mover-coupled" ${m.coupledSync === false ? '' : 'checked'}></tf-toggle>
      </div>
      <div class="wizard-warning info">${sprite('info')}<div>${escapeHtml(T('elastic.mover_dialog_note'))}</div></div>
      <div class="num-err" id="nas-mover-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="save" data-action="confirm">${escapeHtml(T('schedule.save'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  wireScheduleFields(win, 'nas-mover', schedule);
  // An unrecognised stored value falls back to the default rather than being
  // added as a silent fifth option nobody offered.
  const age = Number(m.minAgeSecs);
  win.querySelector('#nas-mover-age').setOptions(
    MOVER_AGE_OPTIONS.map((v) => ({ value: String(v), label: v === 0 ? T('elastic.mover_age_none') : fmtDuration(v) })),
    String(MOVER_AGE_OPTIONS.includes(age) ? age : 7200),
  );
  const free = Number(m.cacheMinFreePct);
  win.querySelector('#nas-mover-free').setOptions(
    MOVER_FREE_OPTIONS.map((v) => ({ value: String(v), label: `${v}%` })),
    String(MOVER_FREE_OPTIONS.includes(free) ? free : 20),
  );
  let busy = false;
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    busy = true;
    const btn = win.querySelector('[data-action="confirm"]');
    btn.setAttribute('disabled', '');
    try {
      await screen.nas('tentaNasElasticMoverScheduleSetRequest', {
        name: array.name,
        enabled: Boolean(win.querySelector('#nas-mover-enabled').checked),
        schedule: readScheduleFields(win, 'nas-mover'),
        minAgeSecs: Number(win.querySelector('#nas-mover-age').value),
        cacheMinFreePct: Number(win.querySelector('#nas-mover-free').value),
        coupledSync: Boolean(win.querySelector('#nas-mover-coupled').checked),
      });
      toast(T('schedule.saved'), 'success');
      win.close(true);
      if (onDone) onDone();
    } catch (err) {
      busy = false;
      btn.removeAttribute('disabled');
      const errEl = win.querySelector('#nas-mover-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  return win;
}

const ELASTIC_CADENCE_DEFAULT = {
  sync: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 },
  scrub: { every: 'weekly', hour: 4, minute: 0, weekday: 0, day: 1 },
};

/**
 * The two SnapRAID cadences. Plain schedules with no extra settings, so they
 * reuse the shared editor the scrub and TRIM of a pool use.
 *
 * Takes the cadence and its switch rather than a whole array, because n15 has
 * only the schedule row and n11 has only `snapraid` — neither has the other's
 * shape.
 */
export function openElasticScheduleEditor(screen, { name, kind, schedule, enabled }, onDone) {
  openScheduleEditor({
    title: T(`elastic.${kind}_schedule_title`, { name }),
    icon: kind === 'sync' ? 'refresh' : 'search',
    schedule: schedule || ELASTIC_CADENCE_DEFAULT[kind],
    enabled: Boolean(enabled),
    allowed: kind === 'sync' ? ['daily', 'weekly'] : ['weekly', 'monthly'],
    note: T(`elastic.${kind}_schedule_note`),
    onSave: async ({ enabled: on, schedule: next }) => {
      const request = kind === 'sync' ? 'tentaNasElasticSyncScheduleSetRequest' : 'tentaNasElasticScrubScheduleSetRequest';
      await screen.nas(request, { name, enabled: on, schedule: next });
      toast(T('schedule.saved'), 'success');
      if (onDone) onDone();
    },
  });
}

// The pane's structure, and nothing about the array's current state: it is a
// function of the admin flag and the visit's array name only, so every poll
// yields the same string and the keyed patch in `draw` keeps the pane as it
// is. What the array reports is written into it by `paintPane`.
function paneSkeletonHtml(name, admin) {
  return `<div class="stack" data-pane="elastic">
    <div class="kpi" data-part="kpi"></div>
    <div class="section-card"><div class="section-card-head"><div class="title">${sprite('cylinder')} ${escapeHtml(T('elastic.disks'))}</div><span class="hint">${escapeHtml(T('elastic.independent_fs'))}</span></div>
      <div class="vdev-group" data-group="data"><div class="vg-head"><span class="vg-type">${escapeHtml(T('elastic.data'))} · MERGERFS</span><span class="mono" data-f="union-path"></span><span class="hint">${escapeHtml(T('elastic.policy'))}: <span data-f="create-policy"></span></span>${admin ? `<span class="actions"><span ${SLOT} data-slot="add-disk"></span><span ${SLOT} data-slot="add-disk-undo"></span></span>` : ''}</div>
        <div class="disk-cells" data-part="data-cells"></div>
        <div ${SLOT} data-slot="add-disk-pending"></div>
        <div ${SLOT} data-slot="add-disk-reason"></div></div>
      <div class="vdev-group" data-group="parity"><div class="vg-head"><span class="vg-type">PARITY · SNAPRAID</span></div><div class="disk-cells" data-part="parity-cells"></div><div ${SLOT} data-slot="no-parity"></div></div>
      <div class="vdev-group" data-group="cache"><div class="vg-head"><span class="vg-type">${escapeHtml(T('elastic.cache'))}</span><span class="hint">${escapeHtml(T('elastic.cache_no_protection'))}</span></div><div class="disk-cells" data-part="cache-cells"></div><div ${SLOT} data-slot="cache-foot"></div></div>
    </div>
    ${foldersSkeletonHtml(admin)}
    <div class="grid-2"><div class="section-card nas-elastic-state"><div class="section-card-head"><div class="title">${sprite('shield')} ${escapeHtml(T('elastic.state'))}</div><tf-chip dot data-f="state-chip"></tf-chip></div>
      <div class="stat-rows">${rowSkel(T('elastic.mountpoint'), 'state-path')}${rowSkel(T('elastic.state'), 'state-detail')}<div class="sr"><span class="k">${escapeHtml(T('elastic.unprotected_bytes'))}</span><span class="v" data-fig="moved-unsynced"></span></div><div class="sr"><span class="k">${escapeHtml(T('elastic.updated'))}</span><span class="v" data-fig="updated"></span></div></div>
      <div ${SLOT} data-slot="next-steps"></div>
      <div class="explain-box mt-md">${escapeHtml(T('elastic.restore_hint'))}</div>
      <div ${SLOT} data-slot="restore"></div>
      ${admin ? '' : `<div class="hint mt-sm">${escapeHtml(T('elevation.admin_only'))}</div>`}
      <div ${SLOT} data-part="message"></div>
    </div><div class="section-card nas-snapraid"><div class="section-card-head"><div class="title">${sprite('shield')} SnapRAID</div><div class="actions">
      <tf-button variant="secondary" size="sm" icon="refresh" data-act="sync">${escapeHtml(T('elastic.sync_now'))}</tf-button><tf-button variant="ghost" size="sm" icon="search" data-act="scrub">${escapeHtml(T('elastic.scrub_now'))}</tf-button></div></div>
      <div ${SLOT} data-slot="maintenance-reason"></div><div ${SLOT} data-slot="sync-confirm"></div><div ${SLOT} data-slot="repair-unavailable"></div>
      <div class="stat-rows" data-part="snapraid-rows"></div>
      <div class="explain-box mt-md">${escapeHtml(T('elastic.snapshot_only'))}</div><div class="hint mt-sm">${escapeHtml(T('elastic.maintenance_hint'))}</div>
      <div class="nas-snapraid-history mt-md"><div class="title">${escapeHtml(T('elastic.history'))}</div><div ${SLOT} data-slot="history-list"></div><div ${SLOT} data-slot="history-empty"></div></div>
    </div></div>
    ${moverSkeletonHtml(admin)}
    ${admin ? `<div class="section-card danger-zone"><h4>${sprite('alert')} ${escapeHtml(T('danger.title'))}</h4>
      ${dangerRowHtml({ title: T('elastic.dissolve', { name }), desc: T('elastic.dissolve_desc'), action: T('elastic.dissolve_action'), icon: 'trash', act: 'destroy' })}
    </div>` : ''}
  </div>`;
}

// The bytes a cache holds outside parity, when that is the fact the
// "Ochrona" tile has to lead with (n11: "18 GiB na cache", warning colour).
// Only an array WITH parity and a cache has such bytes to report; with no
// parity every byte is outside it and the tile says so in words instead.
export function cacheWaitingBytes(array) {
  const bytes = array.protection?.cacheUnprotectedBytes;
  if (!(array.parityDisks || []).length || !(array.cacheDisks || []).length || !knownBytes(bytes)) return null;
  return Number(bytes) > 0 ? Number(bytes) : null;
}

// n11: "Błędy parity (30 dni)". The window is the node's, so the label says
// which one the count covers; an absent window keeps the bare label rather
// than inventing one.
function parityErrorsLabel(snapraid) {
  const days = Number(snapraid?.parityErrorsWindowDays);
  return Number.isFinite(days) && days > 0
    ? T('elastic.parity_errors_window', { n: days })
    : T('elastic.parity_errors');
}

export async function drawElasticDetail(screen, body) {
  const name = screen.array;
  const sourceNodeId = screen.currentNode()?.nodeId;
  const view = document.createElement('div');
  view.className = 'stack nas-elastic-detail';
  // The heading is the same for the whole visit and is written once. The
  // pane below it is `[data-part="main"]`, a keyed host that holds the error,
  // the loading line or the pane skeleton — so a failed read swaps the pane
  // out and a good one puts it back, and nothing else ever replaces it.
  // The shell owns the one breadcrumb of the node view ("TentaNas › helios ›
  // Pule › media", n11); this view only names its tail, exactly as the ZFS
  // pool detail does. Set before the first read, so a failed one still has
  // the way back.
  screen.setCrumbTail?.(poolCrumbTail(screen.nodeId, name));
  view.innerHTML = `<div class="section-card-head nas-elastic-heading"><div class="title">${sprite('layers')} <span class="mono">${escapeHtml(name)}</span> <tf-chip status="accent" label="Elastic Array"></tf-chip></div><div class="actions">
      <tf-button variant="secondary" icon="refresh" data-act="refresh">${escapeHtml(T('elastic.refresh'))}</tf-button></div></div>
    <div data-part="main" ${SLOT}></div>`;
  body.replaceChildren(view);
  const main = view.querySelector('[data-part="main"]');
  const isCurrent = () => !screen.disposed && view.isConnected && screen.currentNode()?.nodeId === sourceNodeId && screen.array === name;
  let epoch = 0;
  let array = null;
  let busy = false;
  let submitted = false;
  let message = '';
  let submittedJobId = null;
  let moverOpen = false;
  const expanded = new Set();
  // A cause the helper records as one only a Restore addresses ('other' — a
  // journal an older helper left wedged included, which the helper settles
  // on this very Restore) offers it whatever the history holds.
  const canRestore = () => array && array.enabled && !['active', 'creating'].includes(array.state)
    && (array.attention === 'other'
      || !(array.snapraid?.history || []).some((run) => ['sync', 'scrub'].includes(run.kind) && ['running', 'failed', 'needs_attention'].includes(run.outcome)));
  const maintenanceReason = () => elasticMaintenanceBlocker(array, screen.isAdmin);
  // The mover needs no parity — the helper skips the coupled sync on an array
  // without one — but it does need something to move, so a cacheless array is
  // refused outright rather than offered a run that would walk nothing.
  // `unresolvedOperation` is its own fact and NOT derivable from the SnapRAID
  // history: the blocking row may be a mover's, and it outlives the array
  // returning to 'active' after a Restore. Without it the button would be
  // offered for a request the node can only refuse.
  // A repair may start on an array that NEEDS ATTENTION — that is the state it
  // exists to resolve — so it deliberately does not share `maintenanceReason`,
  // which requires an active one.
  const repairReason = () => !screen.isAdmin ? T('elevation.admin_only')
    : !(array?.parityDisks || []).length ? T('elastic.no_parity')
      : !array?.enabled || !['active', 'needs_attention'].includes(array.state) ? T('elastic.repair_not_ready')
        : (array.snapraid?.history || []).some((run) => run.outcome === 'running') ? T('elastic.run_running')
          // CAN it run, before WHETHER it should — and in that order, so a
          // missing disk is named as the thing to fix first rather than
          // reported as "nothing to repair".
          : repairBlocker(array) || repairHeld(array) || (!repairEvidence(array) ? T('elastic.repair_none') : '');
  // ONE reason for the whole array, rendered on every data disk. An array
  // with no parity evidence gets no repair control at all, so the button
  // cannot be the thing that suggests a repair is due; and an array a repair
  // could not run on gets none either, with `repairReason` saying why.
  const repairFor = () => (array && !repairReason() ? repairEvidence(array) : '');
  // An add that stopped part-way is FINISHED from here, on an array that
  // needs attention too — that add is what it needs attention for — and no
  // other disk is offered while it stands (F3).
  const addDiskReason = () => !screen.isAdmin ? T('elevation.admin_only')
    : !array?.enabled || !(pendingAdd(array) ? ['active', 'needs_attention'].includes(array.state) : array.state === 'active') ? T('elastic.maintenance_not_ready')
      : !pendingAdd(array) && array.unresolvedOperation ? T('elastic.add_disk_unresolved')
        : (array.snapraid?.history || []).some((run) => run.outcome === 'running') ? T('elastic.run_running')
          : array.mover?.lastRun?.outcome === 'running' ? T('elastic.mover_running') : '';
  // An array whose only unresolved operations are mover runs is offered the
  // mover: the next run is what settles them, and after the node's own
  // attempts stop it is the admin's way on.
  const moverReason = () => !screen.isAdmin ? T('elevation.admin_only')
    : !(array?.cacheDisks || []).length ? T('elastic.mover_no_cache')
      : !array.enabled || (array.state !== 'active' && !array.moverSettlesUnresolved) ? T('elastic.maintenance_not_ready')
        : array.unresolvedOperation && !array.moverSettlesUnresolved ? T('elastic.mover_unresolved')
          : (array.snapraid?.history || []).some((run) => run.outcome === 'running') ? T('elastic.run_running')
            : array.mover?.lastRun?.outcome === 'running' ? T('elastic.mover_running') : '';

  // THE OWNER'S RULE: a poll patches what changed and never rebuilds a
  // subtree. `main` holds at most an error and the pane; the pane's skeleton
  // is a pure function of the visit, so an ordinary poll matches its key and
  // markup and keeps every node — the KPI tiles, the disk cells, an open
  // <details>, a button under the cursor. Only a failed read (the pane goes)
  // or an admin flag that flipped (a different skeleton) builds it anew.
  const draw = (error = '') => {
    if (!isCurrent()) return;
    patchKeyedList(main, [
      ...(error ? [{ key: 'error', html: `<tf-alert tone="danger" title="${escapeAttr(T('load_failed'))}" message="${escapeAttr(error)}"></tf-alert>` }] : []),
      ...(array ? [{ key: 'pane', html: paneSkeletonHtml(name, screen.isAdmin) }]
        : error ? [] : [{ key: 'loading', html: `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>` }]),
    ]);
    const pane = main.querySelector('[data-pane="elastic"]');
    if (!pane || !array) return;
    if (!pane.__tfWired) {
      pane.__tfWired = true;
      // A fresh pane opens the mover section the way the admin left it; one
      // that survives a poll keeps its own state and is never touched here.
      const mover = pane.querySelector('details[data-section="mover"]');
      mover.open = moverOpen;
      mover.addEventListener('toggle', () => { if (isCurrent() && mover.isConnected) moverOpen = mover.open; });
    }
    paintPane(pane);
  };

  // Every value the array reports, written into the pane's existing nodes.
  // setAttr/setText touch a node only when its value differs, and the keyed
  // lists add or remove only the disk, folder or run that came or went.
  const paintPane = (pane) => {
    const admin = screen.isAdmin;
    const status = elasticState(array);
    const lock = busy || submitted;
    const maintenanceBlocked = maintenanceReason();
    const moverBlocked = moverReason();
    const addDiskBlocked = addDiskReason();
    const repair = repairFor();
    // A repair that CANNOT RUN has to say so where the admin is looking for
    // it. "Nothing to repair" and "you are not an admin" are not news and stay
    // silent; a data disk that is missing or unmounted is the one thing they
    // have to act on before a repair is possible at all, and it would
    // otherwise show only as an absent button.
    const repairUnavailable = admin ? repairBlocker(array) : '';
    const cacheWaiting = cacheWaitingBytes(array);

    paintStatCards(pane.querySelector('[data-part="kpi"]'), [
      { key: 'capacity', attrs: { icon: 'cylinder', 'data-fig': 'capacity', label: T('elastic.capacity'), value: fmtOptionalBytes(array.usedBytes), suffix: '/ ' + fmtOptionalBytes(array.usableBytes) } },
      // n11 leads the tile with the bytes waiting on the cache, in the warning
      // colour, whenever there are any: that is how much a disk failure right
      // now could cost. With nothing waiting the tile says the checkpoint's
      // state and when it was taken.
      { key: 'protection', attrs: cacheWaiting != null
        ? { icon: 'shield', 'data-fig': 'protection', label: T('elastic.protection'), value: fmtOptionalBytes(cacheWaiting), delta: T('elastic.cache_pending'), 'delta-type': 'warn', accent: 'warning' }
        : { icon: 'shield', 'data-fig': 'protection', label: T('elastic.protection'), value: protectionLabel(array), delta: `${T('elastic.last_sync')}: ${fmtDate(array.protection?.protectedAsOf)}`, 'delta-type': null, accent: null } },
      { key: 'parity', attrs: { icon: 'database', label: T('elastic.parity'), value: String((array.parityDisks || []).length), delta: T('elastic.tolerance', { n: array.protection?.faultTolerance ?? '—' }) } },
      { key: 'cache', attrs: { icon: 'database', 'data-fig': 'cache', label: T('elastic.cache'), value: fmtOptionalBytes(array.cacheUsedBytes), suffix: '/ ' + fmtOptionalBytes(array.cacheSizeBytes) } },
    ]);

    // Disks. A member with no kernel name gets no repair button: the admin
    // confirms by retyping the name on the cell, and "brak dysku" is not a
    // disk anyone can confirm overwriting.
    setText(field(pane, 'union-path'), array.unionPath || '');
    setText(field(pane, 'create-policy'), createPolicyLabel(array.createPolicy));
    // ONE button slot: "Dodaj dysk" on a whole array, "Dokończ dodawanie
    // dysku <name>" while an add stands unfinished — keyed, so a poll keeps
    // whichever is there and only the add starting or ending swaps it.
    const pending = pendingAdd(array);
    const addHost = pane.querySelector('[data-slot="add-disk"]');
    if (addHost) {
      const addDisk = slotEl(addHost, true, pending ? 'resume' : 'add', pending
        ? '<tf-button variant="primary" size="sm" icon="play" data-act="add-disk-resume"></tf-button>'
        : `<tf-button variant="secondary" size="sm" icon="plus" data-act="add-disk">${escapeHtml(T('elastic.add_disk'))}</tf-button>`);
      const resumeLabel = pending ? T('elastic.add_disk_resume', { disk: pendingAddName(pending) }) : '';
      if (pending) setAttr(addDisk, 'label', resumeLabel);
      setAttr(addDisk, 'disabled', lock || Boolean(addDiskBlocked));
      setAttr(addDisk, 'title', addDiskBlocked || resumeLabel || T('elastic.add_disk_online'));
      // The undo only while the node says the disk never joined the share.
      const undo = slotEl(pane.querySelector('[data-slot="add-disk-undo"]'), Boolean(pending?.undoPossible), 'undo',
        `<tf-button variant="ghost" size="sm" icon="rotate" data-act="add-disk-undo">${escapeHtml(T('elastic.add_disk_undo'))}</tf-button>`);
      if (undo) {
        setAttr(undo, 'disabled', lock || Boolean(addDiskBlocked));
        setAttr(undo, 'title', T('elastic.add_disk_undo_title', { disk: pendingAddName(pending), name: array.name }));
      }
    }
    const pendingNote = slotEl(pane.querySelector('[data-slot="add-disk-pending"]'), Boolean(pending), 'pending',
      '<div class="hint nas-add-pending"><span data-f="add-pending"></span> <span data-f="add-step"></span> <span data-f="add-undo-note"></span></div>');
    if (pendingNote) {
      setText(field(pendingNote, 'add-pending'), T('elastic.add_disk_pending', { disk: pendingAddName(pending) }));
      setText(field(pendingNote, 'add-step'), pendingStepText(pending));
      setText(field(pendingNote, 'add-undo-note'), pending.undoPossible ? '' : T('elastic.add_disk_undo_unavailable'));
    }
    const addHint = slotEl(pane.querySelector('[data-slot="add-disk-reason"]'), admin && Boolean(addDiskBlocked), 'hint', '<div class="hint"></div>');
    if (addHint) setText(addHint, addDiskBlocked);
    paintDiskCells(pane.querySelector('[data-part="data-cells"]'), array.dataDisks || [], (d) => d.filesystem || array.filesystem, (d) => (d.diskName ? repair : ''));
    paintDiskCells(pane.querySelector('[data-part="parity-cells"]'), array.parityDisks || [], () => array.filesystem);
    slotEl(pane.querySelector('[data-slot="no-parity"]'), !(array.parityDisks || []).length, 'none', `<div class="hint">${escapeHtml(T('elastic.no_parity'))}</div>`);
    paintDiskCells(pane.querySelector('[data-part="cache-cells"]'), array.cacheDisks || [], () => array.filesystem);
    const hasCache = (array.cacheDisks || []).length > 0;
    patchKeyedList(pane.querySelector('[data-slot="cache-foot"]'), hasCache
      ? [{ key: 'pending', html: cachePendingHtml() }]
      : [{ key: 'none', html: `<div class="hint">${escapeHtml(T('elastic.cache_none'))}</div>` }]);
    setText(pane.querySelector('[data-fig="cache-pending"]'), fmtOptionalBytes(array.protection?.cacheUnprotectedBytes));

    paintFolders(pane.querySelector('.nas-folders'), array, admin);

    // Array state.
    const chip = field(pane, 'state-chip');
    setAttr(chip, 'status', status.tone);
    setAttr(chip, 'label', status.label);
    setText(field(pane, 'state-path'), array.unionPath || '');
    setText(field(pane, 'state-detail'), elasticStateDetail(array) || status.label);
    setAttr(field(pane, 'state-detail'), 'title', elasticStateTitle(array));
    // F2: on a parity fault a Sync would pay for, what to do next and in
    // which order — the repair while a scrub's marks wait for it, then a
    // scrub that re-measures, and the Sync last, behind its confirm.
    const needsConfirm = syncNeedsAcknowledgement(array) && (array.parityDisks || []).length > 0;
    const steps = slotEl(pane.querySelector('[data-slot="next-steps"]'), needsConfirm, 'steps',
      `<div class="explain-box mt-md nas-next-steps"><div class="title">${escapeHtml(T('elastic.next_steps'))}</div><ol data-part="steps"></ol></div>`);
    if (steps) {
      patchKeyedList(steps.querySelector('[data-part="steps"]'), [
        ...(repairEvidence(array) ? [{ key: 'repair', html: `<li>${escapeHtml(T('elastic.step_repair'))}</li>` }] : []),
        { key: 'scrub', html: `<li>${escapeHtml(T('elastic.step_scrub'))}</li>` },
        { key: 'sync', html: `<li>${escapeHtml(T('elastic.step_sync'))}</li>` },
      ]);
    }
    setText(pane.querySelector('[data-fig="moved-unsynced"]'), fmtOptionalBytes(array.protection?.movedUnsyncedBytes));
    setText(pane.querySelector('[data-fig="updated"]'), fmtDate(array.updatedAt));
    const restore = slotEl(pane.querySelector('[data-slot="restore"]'), admin && Boolean(canRestore()), 'restore', `<tf-button variant="secondary" class="mt-md" data-act="restore">${escapeHtml(T('elastic.restore'))}</tf-button>`);
    if (restore) setAttr(restore, 'disabled', lock);
    const note = pane.querySelector('[data-part="message"]');
    patchKeyedList(note, message ? [
      { key: 'text', html: '<div class="explain-box mt-md" role="status"></div>' },
      { key: 'jobs', html: `<tf-button variant="ghost" data-act="jobs">${escapeHtml(T('elastic.jobs'))}</tf-button>` },
    ] : []);
    if (message) setText(note.firstElementChild, message);

    // SnapRAID.
    for (const act of ['sync', 'scrub']) setAttr(pane.querySelector(`.nas-snapraid [data-act="${act}"]`), 'disabled', lock || Boolean(maintenanceBlocked));
    const reasonHint = slotEl(pane.querySelector('[data-slot="maintenance-reason"]'), Boolean(maintenanceBlocked), 'hint', '<div class="hint mb-sm"></div>');
    if (reasonHint) setText(reasonHint, maintenanceBlocked);
    slotEl(pane.querySelector('[data-slot="sync-confirm"]'), !maintenanceBlocked && syncNeedsAcknowledgement(array), 'hint',
      `<div class="hint mb-sm">${escapeHtml(T('elastic.sync_fault_hint'))}</div>`);
    const repairHint = slotEl(pane.querySelector('[data-slot="repair-unavailable"]'), Boolean(repairUnavailable), 'hint', '<div class="hint mb-sm"></div>');
    if (repairHint) setText(repairHint, repairUnavailable);
    const snapraid = array.snapraid || {};
    const rows = pane.querySelector('[data-part="snapraid-rows"]');
    patchKeyedList(rows, [
      { key: 'last-sync', html: rowSkel(T('elastic.last_sync'), 'sr-last-sync') },
      { key: 'last-scrub', html: rowSkel(T('elastic.last_scrub'), 'sr-last-scrub') },
      { key: 'sync-schedule', html: pillRowSkel(T('elastic.sync_schedule'), 'sr-sync-schedule', 'sync-schedule', admin) },
      { key: 'scrub-schedule', html: pillRowSkel(T('elastic.scrub_schedule'), 'sr-scrub-schedule', 'scrub-schedule', admin) },
      // n11 repeats the cache figure on this card, beside the parity it is
      // outside of. Only an array with a cache has the row at all.
      ...(hasCache ? [{ key: 'cache-pending', html: rowSkel(T('elastic.cache_pending'), 'sr-cache-pending') }] : []),
      { key: 'parity-errors', html: '<div class="sr"><span class="k" data-f="sr-parity-errors-label"></span><span class="v" data-f="sr-parity-errors"></span></div>' },
      { key: 'config', html: rowSkel(T('elastic.config'), 'sr-config') },
    ]);
    setText(field(rows, 'sr-last-sync'), fmtDate(array.protection?.protectedAsOf));
    setText(field(rows, 'sr-last-scrub'), fmtDate(snapraid.lastScrub?.finishedAt));
    setText(field(rows, 'sr-sync-schedule'), cadenceValue(snapraid.syncSchedule, snapraid.syncScheduleEnabled));
    setText(field(rows, 'sr-scrub-schedule'), cadenceValue(snapraid.scrubSchedule, snapraid.scrubScheduleEnabled));
    const cacheRow = field(rows, 'sr-cache-pending');
    if (cacheRow) {
      setText(cacheRow, fmtOptionalBytes(array.protection?.cacheUnprotectedBytes));
      setClass(cacheRow, 'num-warn', cacheWaiting != null);
    }
    setText(field(rows, 'sr-parity-errors-label'), parityErrorsLabel(snapraid));
    setText(field(rows, 'sr-parity-errors'), snapraid.parityErrors ?? '—');
    setText(field(rows, 'sr-config'), snapraid.configPath || '—');
    paintHistory(pane.querySelector('.nas-snapraid-history'), snapraid.history || [], expanded);

    paintMover(pane.querySelector('details[data-section="mover"]'), array, lock || Boolean(moverBlocked), moverBlocked);
    setAttr(pane.querySelector('.danger-zone [data-act="destroy"]'), 'disabled', lock);
  };

  // ONE click listener for the visit. Buttons come and go with the data (a
  // repair on a faulted disk, Restore on a pending array), and a listener per
  // button would have to be re-bound on exactly the polls that create one and
  // never twice on the ones that do not. Everything it acts on is read from
  // the CURRENT `array` at click time, never captured when a button was made:
  // a poll between the render and the click may have changed it, and a dialog
  // must open on the state the admin is looking at.
  view.addEventListener('click', (event) => {
    if (!isCurrent()) return;
    const target = event.target;
    const el = target.closest?.('[data-act]');
    if (!el || !view.contains(el) || el.hasAttribute('disabled')) return;
    const act = el.dataset.act;
    switch (act) {
      case 'refresh': refresh(); return;
      case 'jobs': screen.switchTab('jobs'); return;
      case 'disk': screen.openDisk(el.closest('[data-disk]').dataset.disk); return;
      // A Sync over an unrepaired fault goes through the confirm that names
      // its cost; it is the only way the acknowledgement is ever sent.
      case 'sync':
        if (array && !maintenanceReason() && syncNeedsAcknowledgement(array)) {
          openSyncOverFaultDialog(screen, array, refresh);
          return;
        }
        execute(act);
        return;
      case 'restore': case 'scrub': case 'mover': execute(act); return;
      case 'history-job': screen.openJobLog(el.dataset.job, finishJob); return;
      default: break;
    }
    if (!array) return;
    switch (act) {
      case 'fix': {
        if (repairReason()) return;
        const branch = el.closest('[data-branch]')?.dataset.branch;
        const disk = (array.dataDisks || []).find((d) => d.name === branch);
        if (disk) openElasticFixDialog(screen, array, disk, repairFor(), refresh);
        return;
      }
      case 'add-disk': if (!addDiskReason() && !pendingAdd(array)) openAddDataDiskDialog(screen, array, refresh); return;
      case 'add-disk-resume': if (!addDiskReason()) openResumeAddDiskDialog(screen, array, refresh); return;
      case 'add-disk-undo': if (!addDiskReason()) openUndoAddDiskDialog(screen, array, refresh); return;
      case 'destroy': openElasticDestroyDialog(screen, array, () => screen.openArray(null)); return;
      // The header button AND the pill both carry this action; delegation
      // serves both, so neither can be the one left unbound.
      case 'mover-schedule': openMoverScheduleEditor(screen, array, refresh); return;
      case 'sync-schedule':
      case 'scrub-schedule': {
        const kind = act === 'sync-schedule' ? 'sync' : 'scrub';
        const s = array.snapraid || {};
        openElasticScheduleEditor(screen, {
          name,
          kind,
          schedule: kind === 'sync' ? s.syncSchedule : s.scrubSchedule,
          enabled: kind === 'sync' ? s.syncScheduleEnabled : s.scrubScheduleEnabled,
        }, refresh);
        return;
      }
      case 'folder-cache': {
        if (!screen.isAdmin) return;
        const folder = (array.folders || []).find((f) => f.name === el.dataset.folder);
        if (folder) openFolderCacheDialog(screen, array, folder, refresh);
        return;
      }
      default:
    }
  });
  // `toggle` does not bubble, so the history's <details> are followed in the
  // capture phase: one listener for every run row, present and future.
  view.addEventListener('toggle', (event) => {
    const details = event.target;
    if (!isCurrent() || !details?.matches?.('details[data-run]') || !details.isConnected) return;
    if (details.open) expanded.add(details.dataset.run); else expanded.delete(details.dataset.run);
  }, true);

  const finishJob = async (job) => {
    if (!isCurrent()) return;
    const terminal = ['succeeded', 'failed'].includes(job?.status);
    if (terminal) message = T('elastic.job_finished');
    const refreshed = await refresh();
    if (terminal && refreshed && isCurrent() && submittedJobId && job.jobId === submittedJobId) {
      submitted = false;
      submittedJobId = null;
      draw();
    }
  };

  const refresh = async () => {
    if (!isCurrent()) return;
    const request = ++epoch;
    try {
      const result = await screen.nas('tentaNasElasticArrayGetRequest', { name });
      if (!isCurrent() || request !== epoch) return;
      if (!result.array || result.array.name !== name || result.array.kind !== 'elastic-array') throw new Error(T('elastic.bad_response'));
      array = result.array;
      draw();
      return true;
    } catch (error) {
      if (isCurrent() && request === epoch) { array = null; draw(errMessage(error)); }
    }
  };

  const execute = async (action) => {
    if (!isCurrent() || !screen.isAdmin || busy || submitted) return;
    const blocked = action === 'restore' ? () => !canRestore() : action === 'mover' ? moverReason : maintenanceReason;
    const allowed = () => isCurrent() && screen.isAdmin && !blocked();
    if (!allowed()) return;
    busy = true;
    draw();
    let sent = false;
    try {
      const result = await screen.withSudo((sudoPassword) => {
        if (!allowed()) return null;
        sent = true;
        submitted = true;
        return screen.nas(ACTION_REQUEST[action], { name, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS });
      }, T(ACTION_TITLE[action]), allowed);
      if (!isCurrent()) return;
      if (result?.job?.jobId) {
        message = T(ACTION_ACCEPTED[action]);
        if (action !== 'restore') submittedJobId = result.job.jobId;
        screen.openJobLog(result.job.jobId, finishJob);
      } else if (result?.approval?.requestId) message = T(ACTION_APPROVAL[action]);
      else if (sent) message = T('elastic.request_unknown');
    } catch (error) {
      if (isCurrent()) message = sent ? T('elastic.request_unknown') : errMessage(error);
    } finally {
      busy = false;
      if (isCurrent()) { draw(); if (sent) await refresh(); }
    }
  };

  const poll = async () => { await refresh(); if (isCurrent()) screen.later(poll, POLL_POOLS_MS); };
  draw();
  await poll();
}
