// ===== File: modules/tentanas/pool-detail.js — one pool (n06): KPIs, inner tabs (topology, datasets, snapshots, stats, properties), scrub card, vdev actions, pool properties, danger zone =====
//
// The detail screen keeps one `PoolGet` result as its state and repaints
// the KPI row and the active inner tab from it on every poll; the
// datasets and snapshots tabs own their own lists and only borrow the pool
// name and the dataset list from here.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import {
  T, sprite, POLL_POOLS_MS, POLL_JOB_MODAL_MS, IO_WINDOW_SECS, ADMIN_TIMEOUT_MS, parseServerTs,
  fmtDate, fmtIn, fmtDuration, fmtBytes, fmtMBps, fmtRatio, pct, healthClass, errMessage,
  layoutLabel, stateTone, stateLabel, fmtSchedule, KIND_BADGE,
} from '/js/modules/tentanas/format.js';
import { isDiskIdShape } from '/js/modules/tentanas/machine-id.js';
import { setAttr, setText, patchHtml, patchKeyedList, paintStatCards, paintJobLog, SLOT, slotEl, setClass } from '/js/modules/tentanas/dom-patch.js';
import { openScheduleEditor } from '/js/modules/tentanas/schedule-editor.js';
import { openRetypeDialog, followResponse, dangerRowHtml, warningHtml } from '/js/modules/tentanas/dialogs.js';
import { scrubAction, trimAction } from '/js/modules/tentanas/pools.js';
import { toggleCellCheckbox } from '/js/modules/tentanas/pool-wizard.js';
import { drawDatasets } from '/js/modules/tentanas/datasets.js';
import { drawSnapshots } from '/js/modules/tentanas/snapshots.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-progress-bar.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-stream-chart.js';
import '/js/components/tf-line-chart.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-select.js';
import '/js/components/tf-input.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-option-row.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-breadcrumb.js';

const INNER_TABS = ['topology', 'datasets', 'snapshots', 'stats', 'properties'];

// Editable properties and their allowed values. Pool properties go through
// `zpool set`, the rest are properties of the root dataset (`zfs set`) and
// can be inherited back to the default.
const POOL_PROPS = {
  autotrim: ['on', 'off'],
  autoexpand: ['on', 'off'],
  autoreplace: ['on', 'off'],
  failmode: ['wait', 'continue', 'panic'],
  comment: null,
};
const DATASET_PROPS = {
  compression: ['zstd', 'lz4', 'gzip', 'off'],
  atime: ['on', 'off'],
  relatime: ['on', 'off'],
  recordsize: ['16K', '64K', '128K', '512K', '1M'],
  sync: ['standard', 'always', 'disabled'],
  xattr: ['sa', 'on', 'off'],
  acltype: ['posix', 'nfsv4', 'off'],
};

export async function drawPoolDetail(screen, body) {
  const name = screen.pool;
  if (!INNER_TABS.includes(screen.poolTab)) screen.poolTab = 'topology';
  body.innerHTML = `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`;
  const state = { name, res: null, disks: [], freeDisks: [], live: null, ioSamples: [] };
  try {
    [state.res, state.disks] = await Promise.all([
      screen.nas('tentaNasPoolGetRequest', { name }),
      loadDisks(screen),
    ]);
    state.freeDisks = freeOf(state.disks);
  } catch (e) {
    if (screen.disposed || !body.isConnected) return;
    body.innerHTML = `
      <div class="stack">
        ${crumbs(name)}
        <tf-alert tone="danger" title="${escapeAttr(T('load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>
      </div>`;
    wireBack(screen, body);
    return;
  }
  if (screen.disposed || !body.isConnected) return;

  body.innerHTML = `
    <div class="stack">
      ${crumbs(name)}
      <div class="kpi" id="nas-pool-kpi"></div>
      <tf-tabs variant="underline" value="${escapeAttr(screen.poolTab)}" id="nas-pool-tabs">
        <tf-tab id="topology" icon="layers">${escapeHtml(T('pool.tab_topology'))}</tf-tab>
        <tf-tab id="datasets" icon="folder" count="0">${escapeHtml(T('pool.tab_datasets'))}</tf-tab>
        <tf-tab id="snapshots" icon="save" count="0">${escapeHtml(T('pool.tab_snapshots'))}</tf-tab>
        <tf-tab id="stats" icon="line-chart">${escapeHtml(T('pool.tab_stats'))}</tf-tab>
        <tf-tab id="properties" icon="settings">${escapeHtml(T('pool.tab_properties'))}</tf-tab>
      </tf-tabs>
      <div id="nas-pool-tab-body"></div>
    </div>`;
  wireBack(screen, body);

  const refresh = async () => {
    if (screen.disposed || !body.isConnected) return;
    try {
      [state.res, state.disks] = await Promise.all([
        screen.nas('tentaNasPoolGetRequest', { name }),
        loadDisks(screen),
      ]);
      state.freeDisks = freeOf(state.disks);
    } catch (e) {
      toast(errMessage(e), 'error');
      return;
    }
    if (screen.disposed || !body.isConnected) return;
    recordIoSample(state);
    // A paint step throwing must not stop the poll loop (MINOR 7): `poll`
    // awaits `refresh`, and an uncaught rejection here would keep it from
    // ever reaching `screen.later`, silently freezing n06 until the tab is
    // reopened. Log it — a real bug must still be visible — and keep polling.
    try {
      paintKpis(body, state);
      if (screen.poolTab === 'topology') paintTopology(screen, body, state, refresh);
      if (screen.poolTab === 'properties') paintPropertiesTab(screen, body, state, refresh);
      if (screen.poolTab === 'stats') pushLiveSample(body, state);
    } catch (e) {
      console.error(e);
    }
  };
  const poll = async () => {
    await refresh();
    if (!screen.disposed && body.isConnected) screen.later(poll, POLL_POOLS_MS);
  };

  body.querySelector('#nas-pool-tabs').addEventListener('change', (e) => {
    if (e.detail.value === screen.poolTab) return;
    screen.poolTab = e.detail.value;
    if (screen.poolTab !== 'datasets') screen.dataset = null;
    screen.setLocation();
    drawInner(screen, body, state, refresh);
  });

  recordIoSample(state);
  paintKpis(body, state);
  drawInner(screen, body, state, refresh);
  screen.later(poll, POLL_POOLS_MS);
}

// The whole disk list stays around: the replace wizard needs model and
// serial of the pool's spares, which `zpool status` does not carry.
async function loadDisks(screen) {
  const r = await screen.nas('tentaNasDisksListRequest', {});
  return r.disks || [];
}
const freeOf = (disks) => disks.filter((d) => d.role === 'free');

// The shell's own breadcrumb already says "TentaNas › node"; this one adds
// the "Pule › tank" tail the mockup shows above the pool header.
const crumbs = (name) => `
  <tf-breadcrumb class="nas-crumbs">
    <tf-breadcrumb-item href="#">${escapeHtml(T('tabs.pools'))}</tf-breadcrumb-item>
    <tf-breadcrumb-item current>${escapeHtml(name)}</tf-breadcrumb-item>
  </tf-breadcrumb>`;

function wireBack(screen, body) {
  body.querySelector('.nas-crumbs').addEventListener('click', (e) => {
    const a = e.target.closest('a');
    if (!a) return;
    e.preventDefault();
    screen.pool = null;
    screen.dataset = null;
    screen.clearTimers();
    screen.setLocation();
    screen.drawTab();
  });
}

// ---------------------------------------------------------------------------
// KPIs
// ---------------------------------------------------------------------------

// "21.8 / 32.1 TiB" as value + suffix: the unit is written once when both
// sides share it, otherwise each side carries its own.
export function capacityParts(usedBytes, usableBytes) {
  const used = fmtBytes(usedBytes);
  const usable = fmtBytes(usableBytes);
  const [usedNum, usedUnit] = used.split(' ');
  return usedUnit === usable.split(' ')[1] ? { value: usedNum, suffix: `/ ${usable}` } : { value: used, suffix: `/ ${usable}` };
}

function paintKpis(body, state) {
  const p = state.res.pool;
  const usedPct = pct(p.usedBytes, p.usableBytes);
  const cap = capacityParts(p.usedBytes, p.usableBytes);
  const io = p.io || {};
  const scan = p.scan || {};
  // "1 ostrzeżenie dysku" (n06:188) counts what the topology cells show: a
  // disk whose SMART health warns inside an ONLINE leaf is a warning too (M3).
  const inventory = inventoryOf(state.disks);
  const diskWarnings = (p.vdevs || []).flatMap((v) => v.disks || []).filter((d) => diskCondition(d, inventoryFor(inventory, d)).tone !== 'ok').length;
  const frag = Math.round(Number(p.fragmentationPct) || 0);
  // Four tiles created once; every later poll writes only the attributes that
  // moved, so a tile keeps its identity while its numbers change.
  paintStatCards(body.querySelector('#nas-pool-kpi'), [
    { key: 'capacity', attrs: {
      label: T('pool.kpi_capacity'), value: cap.value, suffix: cap.suffix, icon: 'database',
      accent: usedPct > 90 ? 'danger' : usedPct > 75 ? 'warning' : null,
      delta: T('pool.kpi_capacity_delta', { pct: usedPct, ratio: fmtRatio(p.compressRatio) }),
    } },
    { key: 'state', attrs: {
      label: T('pool.kpi_state'), value: stateLabel(p.state).toUpperCase(), icon: 'check',
      accent: stateTone(p.state) === 'ok' ? 'success' : stateTone(p.state) === 'warn' ? 'warning' : 'danger',
      delta: T('pool.kpi_state_delta', { e: Number(scan.errors) || 0, w: diskWarnings }),
    } },
    { key: 'iops', attrs: {
      label: T('pool.kpi_iops'), value: String(Math.round((Number(io.readIops) || 0) + (Number(io.writeIops) || 0))), icon: 'zap',
      delta: T('pool.kpi_iops_delta', { r: Math.round(Number(io.readIops) || 0), w: Math.round(Number(io.writeIops) || 0) }),
    } },
    { key: 'fragmentation', attrs: {
      label: T('pool.kpi_fragmentation'), value: String(frag), suffix: '%', icon: 'grid-2x2',
      accent: frag > 50 ? 'warning' : null,
      delta: frag > 50 ? T('pool.frag_high') : T('pool.frag_low'),
      'delta-type': frag > 50 ? 'warn' : 'neutral',
    } },
  ]);

  const tabs = body.querySelector('#nas-pool-tabs');
  setAttr(tabs.querySelector('#datasets'), 'count', String(p.datasetCount ?? 0));
  setAttr(tabs.querySelector('#snapshots'), 'count', String(p.snapshotCount ?? 0));
}

// ---------------------------------------------------------------------------
// Inner tabs
// ---------------------------------------------------------------------------

function drawInner(screen, body, state, refresh) {
  const host = body.querySelector('#nas-pool-tab-body');
  state.live = null;
  // Each inner tab owns this host while it is open, and several of them write
  // it directly rather than through patchHtml. Drop the patch cache on every
  // switch so the incoming pane is always rendered — otherwise a pane whose
  // markup happened to equal the outgoing one's would be skipped as "already
  // there" and the previous tab would stay on screen.
  //
  // ONE HOST, ONE WRITER. `#nas-pool-tab-body` is written directly (not
  // through `patchHtml`) by `paintStats` here, by `drawDatasets` and by
  // `drawSnapshots`. That is safe ONLY because all three are reached only
  // from this switch and each does its write as its first synchronous
  // statement, before any await — this line is what makes their direct
  // `innerHTML =` legal. Calling one of them from `refresh` or a poll instead
  // (the obvious optimization for the stats tab) would skip this
  // invalidation: `__tfHtml` would go on describing markup that is no longer
  // on screen, and the next `patchHtml` carrying that same string would be
  // dropped as a no-op — a pane permanently stuck on stale content.
  //
  // The topology and properties panes are the exception that polls: they
  // build a skeleton once (`buildPane`) and recognise it on later polls by
  // identity (`paneRoot`), so any other tab's write here makes them rebuild
  // on their next visit instead of patching nodes that are gone.
  host.__tfHtml = null;
  switch (screen.poolTab) {
    case 'datasets':
      drawDatasets(screen, host, { pool: state.name, onChange: refresh });
      return;
    case 'snapshots':
      drawSnapshots(screen, host, { pool: state.name, datasets: state.res.datasets || [], onChange: refresh });
      return;
    case 'stats':
      paintStats(body, state);
      return;
    case 'properties':
      paintPropertiesTab(screen, body, state, refresh);
      return;
    default:
      paintTopology(screen, body, state, refresh);
  }
}

// ---------------------------------------------------------------------------
// Pane skeletons (shared by the topology and properties panes)
// ---------------------------------------------------------------------------

// A pane of this screen is built ONCE per visit of its inner tab and then only
// written into. Every poll lands on the same nodes, so a button under the
// cursor, the "Wymuś eksport" checkbox, the properties table and the live
// chart all survive it — the owner's rule is literal: patch what changed,
// never rebuild a subtree on a refresh. Only a vdev or a disk that appears
// or disappears changes the structure, and then only its own group or cell.
//
// `paneRoot` answers the root only while `host` still shows the pane of this
// kind. The other inner tabs write the host with `innerHTML`, which detaches
// the old root, so a stale reference can never pass for the live one.
function paneRoot(host, kind) {
  const root = host.__poolPane;
  return root && root.parentNode === host && root.dataset.pane === kind ? root : null;
}

function buildPane(host, kind, html, screen, state, refresh) {
  // ONE HOST, ONE WRITER: this direct write replaces whatever a patch helper
  // last cached for the host, so its caches go with it.
  host.innerHTML = `<div class="stack" data-pane="${kind}">${html}</div>`;
  host.__tfHtml = null;
  host.__tfKeyed = null;
  const root = host.firstElementChild;
  host.__poolPane = root;
  // One delegated listener for the pane's lifetime. Buttons come and go with
  // the data (a scrub's pause/stop, a disk's online/offline), and a listener
  // per button would have to be re-bound on exactly the polls that create
  // one — and never twice on the ones that do not.
  root.addEventListener('click', (e) => onPaneClick(e, root, screen, state, refresh));
  return root;
}

// The pane's only click handler. Everything it acts on is read from `state`
// at click time — the pool, its vdevs and the free disks move with every
// poll, and a closure captured at build time would act on the first poll's.
function onPaneClick(e, root, screen, state, refresh) {
  const el = e.target.closest?.('[data-act]');
  if (!el || !root.contains(el) || el.hasAttribute('disabled')) return;
  const act = el.dataset.act;
  const p = state.res.pool;
  const free = state.freeDisks;
  const vdevOf = () => (p.vdevs || []).find((x) => x.id === el.dataset.vdev);
  const scrub = /^scrub-(start|pause|resume|stop)$/.exec(act);
  if (scrub) { scrubAction(screen, p.name, scrub[1], refresh); return; }
  const trim = /^trim-(start|suspend|resume|cancel)$/.exec(act);
  if (trim) { trimAction(screen, p.name, trim[1], refresh); return; }
  switch (act) {
    case 'scrub-schedule': openScrubScheduleEditor(screen, p, refresh); return;
    case 'trim-schedule': openTrimScheduleEditor(screen, p, refresh); return;
    case 'add-vdev': openAddVdevDialog(screen, p, el.dataset.role, free, refresh); return;
    case 'expand': {
      const v = vdevOf();
      if (!v) return;
      openPickDiskDialog(screen, {
        title: T('pool.vdev_expand_title', { id: v.id }),
        explain: T('pool.vdev_expand_explain', { layout: layoutLabel(v.kind) }),
        disks: free,
        minBytes: Math.min(...(v.disks || []).map((d) => Number(d.sizeBytes) || 0)),
        confirmLabel: T('pool.vdev_expand'),
        onPick: async (disk) => {
          const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolExpandVdevRequest', { name: p.name, vdevId: v.id, diskId: disk.diskId, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('pool.vdev_expand_title', { id: v.id }));
          followResponse(screen, res, refresh, T('pool.vdev_expand_done'));
          return res !== null;
        },
      });
      return;
    }
    case 'remove-vdev': {
      const v = vdevOf();
      if (v) removeVdev(screen, p, v, refresh);
      return;
    }
    case 'replace': {
      const v = vdevOf();
      const d = (v?.disks || []).find((x) => x.name === el.dataset.device);
      if (d) openReplaceWizard(screen, { pool: p, vdev: v, disk: d, freeDisks: free, disks: state.disks, onDone: refresh });
      return;
    }
    case 'offline':
    case 'online':
    case 'clear':
      deviceAction(screen, p, act, el.dataset.device, refresh);
      return;
    case 'disk': screen.openDisk(el.dataset.disk); return;
    case 'export': exportPool(screen, p, Boolean(root.querySelector('#nas-export-force')?.checked)); return;
    case 'destroy': openPoolDestroyDialog(screen, p, state.res.datasets || [], () => leavePool(screen)); return;
  }
}

async function removeVdev(screen, p, v, refresh) {
  const ok = await TfWindow.confirm({ title: T('pool.vdev_remove'), message: T('pool.vdev_remove_confirm', { id: bareVdevLabel(p.vdevs, v), role: T('pool.role_' + v.role) }), confirmLabel: T('pool.vdev_remove'), cancelLabel: I18n.t('common.cancel'), danger: true });
  if (!ok) return;
  const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolRemoveVdevRequest', { name: p.name, vdevId: v.id, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('pool.vdev_remove'));
  followResponse(screen, res, refresh, T('pool.vdev_remove_done'));
}

async function deviceAction(screen, p, action, device, refresh) {
  if (action === 'offline') {
    const ok = await TfWindow.confirm({ title: T('pool.disk_offline'), message: T('pool.disk_offline_confirm', { device, ft: p.faultTolerance }), confirmLabel: T('pool.disk_offline'), cancelLabel: I18n.t('common.cancel'), danger: true });
    if (!ok) return;
  }
  const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolDeviceStateRequest', { name: p.name, device, action, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('pool.disk_' + action));
  followResponse(screen, res, refresh, T('pool.disk_' + action + '_done', { device }));
}

async function exportPool(screen, p, force) {
  const ok = await TfWindow.confirm({ title: T('danger.export'), message: T('danger.export_confirm', { name: p.name }), confirmLabel: T('danger.export_action'), cancelLabel: I18n.t('common.cancel'), danger: true });
  if (!ok) return;
  const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolExportRequest', { name: p.name, force, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('danger.export'));
  followResponse(screen, res, () => leavePool(screen), T('danger.export_done', { name: p.name }));
}

function leavePool(screen) {
  screen.pool = null;
  screen.clearTimers();
  screen.setLocation();
  screen.drawTab();
}

// ---------------------------------------------------------------------------
// Topology (vdevs, disks, scrub, IO, properties, danger zone)
// ---------------------------------------------------------------------------

// n06 gives every group its own sentence; only a data vdev has a fault
// tolerance to state, and a single-device SLOG would otherwise advertise
// "survives 0 disks failing".
const VDEV_HINTS = {
  spare: 'pool.spare_hint',
  special: 'pool.hint_special',
  log: 'pool.hint_log',
  cache: 'pool.hint_cache',
  dedup: 'pool.hint_dedup',
};

// The IO readout of the topology pane: the label is part of the markup, the
// value is written as text on every poll. The label is resolved through T()
// at RENDER time, not here — a module-level T() would be evaluated before the
// language is loaded and would then never follow a locale switch.
const IO_ROWS = [
  ['throughput', 'pool.io_throughput'],
  ['iops', 'pool.io_iops'],
  ['latency', 'pool.io_latency'],
];

function paintIoRows(host, io) {
  const write = (key, text) => setText(host.querySelector(`[data-io="${key}"]`), text);
  write('throughput', `${fmtMBps(io.readBps)} / ${fmtMBps(io.writeBps)} MB/s`);
  write('iops', `${Math.round(Number(io.readIops) || 0)} / ${Math.round(Number(io.writeIops) || 0)}`);
  write('latency', `${(Number(io.readLatencyMs) || 0).toFixed(1)} / ${(Number(io.writeLatencyMs) || 0).toFixed(1)} ms`);
}

const TONE_RANK = { ok: 0, warn: 1, err: 2 };
const leafErrors = (d) => (Number(d.readErrors) || 0) + (Number(d.writeErrors) || 0) + (Number(d.cksumErrors) || 0);

// `zpool status` knows the leaf, not the device behind it: media kind,
// temperature and SMART health (n06:223-226) come from the node's disk
// inventory, matched by id or by kernel name (a partition leaf carries
// neither on its own).
function inventoryOf(disks) {
  const inventory = new Map();
  for (const d of disks) {
    if (d.diskId) inventory.set(d.diskId, d);
    if (d.name) inventory.set(d.name, d);
  }
  return inventory;
}
const inventoryFor = (inventory, d) => (d.diskId && inventory.get(d.diskId)) || inventory.get(d.name);

/// The condition a topology cell shows for one pool leaf (n06:226, M3). The
/// zpool leaf state alone called a reallocating disk healthy — an ONLINE leaf
/// says nothing about the platters. The tone is the WORSE of the leaf state
/// and the disk's SMART health from the inventory, so a FAULTED leaf stays red
/// on a disk SMART calls fine, and a disk SMART warns about turns amber inside
/// an ONLINE vdev. An unknown SMART health is no signal either way. Leaf error
/// counters make an otherwise clean disk a warning, as the KPI always counted
/// them. Tones are `ok | warn | err`: the `.health-dot` modifiers and values
/// tf-chip's `status` allowlist accepts (anything else renders as `info`).
export function diskCondition(d, inv) {
  const errs = leafErrors(d);
  const leaf = stateTone(d.state);
  const smart = healthClass(inv?.health);
  let tone = leaf;
  if (smart && TONE_RANK[smart] > TONE_RANK[tone]) tone = smart;
  if (errs && tone === 'ok') tone = 'warn';
  // The chip names the WHY: a leaf that is not online says its zpool state;
  // otherwise it names the SMART health grade (M5) — the server's reason text
  // is English free text ("8 reallocated sectors"), so it goes in `title=`
  // only, never on the chip itself.
  let chip = null;
  if (d.state !== 'online') chip = { status: leaf, label: stateLabel(d.state) };
  else if (smart === 'warn' || smart === 'err') {
    chip = { status: smart, label: T('health.' + inv.health), title: inv.healthReason || null };
  }
  return { tone, errs, chip };
}

// For a composite vdev (`raidz2-0`, `mirror-1`, `special-0`…) `id` is a name
// zpool itself generated, so it is safe to show. For a bare leaf — cache,
// log, spare or single-disk data (`kind === 'disk'`) — `pools.rs::leaf()`
// sets `id` to the leaf's own path, which is often a by-id serial or WWN
// path (M4, hard rule: no machine identifier as visible text). Label it by
// role instead, with an ordinal when the pool carries more than one bare
// vdev of that role, and keep the real id in `title=` only.
function bareVdevLabel(vdevs, v) {
  if (v.kind !== 'disk') return v.id;
  const bare = (vdevs || []).filter((x) => x.kind === 'disk' && x.role === v.role);
  const ordinal = bare.length > 1 ? bare.indexOf(v) + 1 : 0;
  const role = T('pool.role_' + v.role);
  return ordinal ? `${role} ${ordinal}` : role;
}

// The per-key markup of one vdev group: only what cannot change while the
// group exists (its id, role, layout and the buttons its role offers). The
// state chip, the hint, the button states and the disk cells are written
// into it afterwards, so a poll never re-parses a group.
function vdevSkeletonHtml(v, admin, vdevs) {
  const raidz = /^raidz/.test(v.kind);
  const removable = v.role === 'cache' || v.role === 'log' || v.role === 'spare';
  // `vg-type` names the layout ("RAIDZ1", "Mirror") and, for a composite
  // non-data vdev, its role too ("Special · Mirror"). A bare leaf already
  // gets its role from `bareVdevLabel` in the mono span right next to it
  // ("Cache 1"), so repeating the role here would print it twice on the
  // same header (MINOR 10) — a bare leaf's `vg-type` shows only the layout.
  return `
    <div class="vdev-group" data-vdev="${escapeAttr(v.id)}">
      <div class="vg-head">
        <span class="vg-type">${escapeHtml(v.role === 'data' || v.kind === 'disk' ? layoutLabel(v.kind) : `${T('pool.role_' + v.role)} · ${layoutLabel(v.kind)}`)}</span>
        <span class="mono text-3" title="${escapeAttr(v.id)}">${escapeHtml(bareVdevLabel(vdevs, v))}</span>
        <span data-slot="state" ${SLOT}></span>
        <span class="hint" data-f="hint"></span>
        <span class="spacer"></span>
        ${admin && raidz ? `<tf-button size="sm" variant="secondary" icon="plus" data-act="expand" data-vdev="${escapeAttr(v.id)}">${escapeHtml(T('pool.vdev_expand'))}</tf-button>` : ''}
        ${admin && removable ? `<tf-button size="sm" variant="ghost" tone="critical" icon="trash" data-act="remove-vdev" data-vdev="${escapeAttr(v.id)}">${escapeHtml(T('pool.vdev_remove'))}</tf-button>` : ''}
      </div>
      <div class="disk-cells"></div>
    </div>`;
}

// A leaf `zpool status` can no longer find prints as its numeric GUID
// (`leaf()` in tentaflow-core/src/tentanas/pools.rs keeps `name` verbatim for
// a non-`/` row) — never show that as a device name (M4, hard rule). A
// missing leaf's kernel name never contains only digits, so that is the
// test. The same leak reaches a REMOVED or FAULTED device whose by-id
// symlink is gone: `kernel_name_of` cannot canonicalise the path, so it
// falls back to the path's basename (`zfs.rs:209-219`), and
// `strip_partition_suffix` only folds the kernel's own naming schemes —
// `sdX`, `nvmeXnY`, `mmcblkX` — never a by-id name, which keeps its
// `-partN` suffix (`zfs.rs:226-229`). That basename is `wwn-0x…-part1` or
// `ata-…_SERIAL-part1`, exactly the disk the replace wizard is for, so it
// gets the same treatment as the GUID shape.
//
// Prefer the real kernel name from the disk inventory when the leaf
// resolves to one there (matched by `disk_id` or by name, same as
// `inventoryFor` elsewhere on this screen); otherwise fall back to a
// translated "missing disk" label. The id goes in `title=` only. The wire
// is not changed by this: the GUID/by-id text stays `d.name` for every
// server request (device actions, replace) — only what is painted on
// screen changes.
//
// `zpool`'s own "was /dev/…" annotation on a removed leaf is not
// parenthesized, so `parse_config_row` — which only captures a note inside
// `(...)` (pools.rs:176-180) — never puts it in `note`. Recovering a kernel
// name by parsing `note` for "was …" would be dead code (unreachable given
// the parser), so this does not attempt it; the note text itself is shown
// verbatim in the sub-line (`d.note`, unrelated to the leaf's name) exactly
// as before.
// The shapes (digit-only GUID, UUID, every by-id/by-path prefix) live in
// `machine-id.js`'s `isDiskIdShape` — a ZFS leaf name is only ever a kernel
// name or a disk id/by-id basename, never a human-chosen name, so the disk
// rule (not the narrower opaque-only rule) applies here. Re-exported under
// this name because that is what every call site below reads it as, and so
// pool-detail.test.js keeps its import unchanged.
// `dm-name-…` / `dm-uuid-…` are by-id LINKS; `dm-0` is a real kernel name (a
// LUKS or LVM device) and must stay visible as the disk it is.
export const isUnresolvedLeafName = isDiskIdShape;
function leafDisplayName(d, inv) {
  if (!isUnresolvedLeafName(d.name)) return d.name;
  const kernelName = inv && inv.name && !isUnresolvedLeafName(inv.name) ? inv.name : null;
  return kernelName || T('elastic.disk_absent');
}
function leafDisplayTitle(d) {
  return isUnresolvedLeafName(d.name) ? d.name : null;
}

function diskSkeletonHtml(d, v, inv) {
  const name = leafDisplayName(d, inv);
  const title = leafDisplayTitle(d);
  return `
    <div class="disk-cell ${v.role === 'spare' ? 'spare' : ''}" data-device="${escapeAttr(d.name)}">
      <div class="dc-main">
        <div class="dc-name"><span class="health-dot"></span><span class="mono"${title ? ` title="${escapeAttr(title)}"` : ''}>${escapeHtml(name)}</span><span data-slot="chip" ${SLOT}></span></div>
        <div class="dc-sub" data-f="sub"></div>
        <div class="dc-sub mono" data-f="errs"></div>
      </div>
      <span data-slot="kind" ${SLOT}></span>
      <div class="dc-actions"></div>
    </div>`;
}

// The icon buttons of one disk cell. Each key's markup is fixed, so a button
// that stays offered across polls is the same element; only the set changes
// (an offline disk trades "offline" for "online", errors add "clear").
function diskButtons(d, v, admin, freeCount) {
  const btn = (act, icon, title, data) => ({ key: act, html: `<tf-button size="sm" variant="ghost" icon="${icon}" data-act="${act}" ${data} title="${escapeAttr(title)}"></tf-button>` });
  const dev = `data-device="${escapeAttr(d.name)}"`;
  const out = [];
  if (admin) {
    if (freeCount && v.role !== 'spare') out.push(btn('replace', 'refresh', T('pool.disk_replace'), `data-vdev="${escapeAttr(v.id)}" ${dev}`));
    if (d.state === 'online') out.push(btn('offline', 'ban', T('pool.disk_offline'), dev));
    else if (d.state === 'offline') out.push(btn('online', 'play', T('pool.disk_online'), dev));
    if (leafErrors(d)) out.push(btn('clear', 'check', T('pool.disk_clear'), dev));
  }
  if (d.diskId) out.push({ ...btn('disk', 'chevron-right', T('disks.details'), `data-disk="${escapeAttr(d.diskId)}"`), key: 'disk:' + d.diskId });
  return out;
}

function paintDiskCell(cell, d, v, ctx) {
  const inv = inventoryFor(ctx.inventory, d);
  const cond = diskCondition(d, inv);
  const bad = d.state !== 'online';
  setClass(cell, 'faulted', bad);
  setClass(cell, 'warn', !bad && cond.tone !== 'ok');
  setClass(cell, 'resilver', bad && ctx.resilvering);
  const dot = cell.querySelector('.health-dot');
  for (const t of ['ok', 'warn', 'err']) setClass(dot, t, cond.tone === t);
  const chip = slotEl(cell.querySelector('[data-slot="chip"]'), Boolean(cond.chip), 'chip', '<tf-chip size="sm"></tf-chip>');
  if (chip) {
    setAttr(chip, 'status', cond.chip.status);
    setAttr(chip, 'label', cond.chip.label);
    setAttr(chip, 'title', cond.chip.title || null);
  }
  setText(cell.querySelector('[data-f="sub"]'), [fmtBytes(d.sizeBytes), inv?.temperatureC == null ? null : `${inv.temperatureC}°C`, d.note || null].filter(Boolean).join(' · '));
  const errs = cell.querySelector('[data-f="errs"]');
  setText(errs, `R ${Number(d.readErrors) || 0} · W ${Number(d.writeErrors) || 0} · CKSUM ${Number(d.cksumErrors) || 0}`);
  setClass(errs, 'num-err', cond.errs > 0);
  // The media badge exists only for a leaf the inventory knows; its kind is
  // part of the key, so a changed kind swaps the one badge, nothing else.
  // `KIND_BADGE` (format.js) is shared with elastic-detail.js (n11) so both
  // screens spell the same wire kind ("HDD"/"NVMe") the same way (MINOR 12).
  const kind = inv ? String(inv.kind) : '';
  const [kindClass, kindLabel] = KIND_BADGE[kind] || [];
  slotEl(cell.querySelector('[data-slot="kind"]'), Boolean(kindLabel), 'kind:' + kind, `<span class="disk-kind ${escapeAttr(kindClass || '')}">${escapeHtml(kindLabel || '')}</span>`);
  patchKeyedList(cell.querySelector('.dc-actions'), diskButtons(d, v, ctx.admin, ctx.free.length));
}

function paintVdevGroup(group, v, ctx) {
  const chip = slotEl(group.querySelector('[data-slot="state"]'), v.state !== 'online', 'chip', '<tf-chip dot></tf-chip>');
  if (chip) {
    setAttr(chip, 'status', stateTone(v.state));
    setAttr(chip, 'label', stateLabel(v.state));
  }
  const hintKey = VDEV_HINTS[v.role];
  setText(group.querySelector('[data-f="hint"]'), hintKey ? T(hintKey, { pool: ctx.pool.name }) : T('pool.tolerance_hint', { n: v.faultTolerance }));
  // RAIDZ expansion is always on the group so the admin learns it exists;
  // without a free disk it is disabled with the reason.
  const expand = group.querySelector('[data-act="expand"]');
  setAttr(expand, 'disabled', !ctx.free.length);
  setAttr(expand, 'title', ctx.free.length ? null : T('pools.no_free_disks'));

  const cellsHost = group.querySelector('.disk-cells');
  const disks = v.disks || [];
  patchKeyedList(cellsHost, disks.length
    ? disks.map((d) => ({ key: 'disk:' + d.name, html: diskSkeletonHtml(d, v, inventoryFor(ctx.inventory, d)) }))
    : [{ key: 'empty', html: `<div class="muted">${escapeHtml(T('pool.vdev_empty'))}</div>` }]);
  const cells = new Map([...cellsHost.children].map((el) => [el.dataset.device, el]));
  for (const d of disks) paintDiskCell(cells.get(d.name), d, v, ctx);
}

function topologySkeletonHtml(screen) {
  const admin = screen.isAdmin;
  // The three shortcuts of the mockup; "Dodaj vdev" opens the dialog with
  // the role select, so log/special/dedup groups are reachable from it.
  const addButton = (role, icon) => `<tf-button size="sm" variant="secondary" icon="${icon}" data-act="add-vdev" data-role="${role}">${escapeHtml(T('pool.add_' + role))}</tf-button>`;
  const row = (icon, label, value) => `<div class="sr"><span class="k">${sprite(icon)} ${escapeHtml(label)}</span><span class="v">${value}</span></div>`;
  return `
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('layers')} ${escapeHtml(T('pool.topology_title'))}</div>
        ${admin ? `<div class="actions">${addButton('data', 'plus')}${addButton('cache', 'zap')}${addButton('spare', 'cylinder')}</div>` : ''}
      </div>
      <div data-part="vdevs"></div>
    </div>
    <div class="grid-2 pool-topology">
      <div class="section-card">
        <div class="section-card-head"><div class="title">${sprite('shield')} ${escapeHtml(T('pool.scrub_title_card'))}</div>
          <div class="actions" data-part="scrub-actions"></div></div>
        <div data-slot="scan-bar"></div>
        <div class="stat-rows">
          ${row('check', T('pool.scrub_last'), '<span data-f="last-when"></span><span data-f="last-sep"></span><span data-f="last-errs"></span>')}
          ${row('clock', T('pool.scrub_schedule'), `<span class="sched-pill" ${admin ? 'data-act="scrub-schedule" role="button"' : ''}>${sprite('clock')} <span data-f="scrub-sched"></span></span> <span class="text-3" data-f="scrub-next"></span>`)}
          ${row('cylinder', T('pool.errors'), '<span class="mono" data-f="pool-errors"></span>')}
          ${row('zap', T('pool.autotrim'), '<span data-f="autotrim"></span>')}
          ${row('zap', T('pool.trim_state'), '<span data-f="trim-label"></span> <span class="mono" data-f="trim-pct"></span> <span class="text-3" data-f="trim-last"></span> <span data-part="trim-buttons" style="display:contents"></span>')}
          ${row('clock', T('pool.trim_schedule'), `<span class="sched-pill" data-f="trim-pill">${sprite('clock')} <span data-f="trim-sched"></span></span> <span class="text-3" data-f="trim-next"></span>`)}
        </div>
      </div>
      <div class="section-card">
        <div class="section-card-head"><div class="title">${sprite('trend')} ${escapeHtml(T('pool.io_title'))}</div><span class="hint">${escapeHtml(T('pool.io_hint'))}</span></div>
        <div class="stat-rows">${IO_ROWS.map(([key, labelKey]) => `<div class="sr"><span class="k">${escapeHtml(T(labelKey))}</span><span class="v mono" data-io="${key}"></span></div>`).join('')}</div>
        <tf-stream-chart id="nas-pool-io-live" class="mt-sm"></tf-stream-chart>
        <div class="live-label"><span class="live-dot"></span>${escapeHtml(T('overview.live_window', { w: fmtDuration(IO_WINDOW_SECS) }))}</div>
      </div>
    </div>
    ${propertiesSectionHtml(screen)}`;
}

// Fixed markup per button key, so a button offered on two consecutive polls
// is the same element (a running scrub's "Zatrzymaj" survives the pause).
const actionButton = (act, variant, icon, labelKey) => ({ key: act, html: `<tf-button variant="${variant}" size="sm" icon="${icon}" data-act="${act}">${escapeHtml(T(labelKey))}</tf-button>` });

function scrubButtons(status) {
  if (status === 'running') return [actionButton('scrub-pause', 'ghost', 'pause', 'pool.scrub_pause'), actionButton('scrub-stop', 'ghost', 'stop', 'pool.scrub_stop')];
  if (status === 'paused') return [actionButton('scrub-resume', 'secondary', 'play', 'pool.scrub_resume'), actionButton('scrub-stop', 'ghost', 'stop', 'pool.scrub_stop')];
  return [actionButton('scrub-start', 'secondary', 'play', 'pool.scrub_now')];
}

function trimButtons(trimState) {
  if (trimState === 'trimming') return [actionButton('trim-suspend', 'ghost', 'pause', 'pool.trim_suspend'), actionButton('trim-cancel', 'ghost', 'stop', 'pool.trim_cancel')];
  if (trimState === 'suspended') return [actionButton('trim-resume', 'secondary', 'play', 'pool.trim_resume'), actionButton('trim-cancel', 'ghost', 'stop', 'pool.trim_cancel')];
  return [actionButton('trim-start', 'secondary', 'play', 'pool.trim_now')];
}

function paintScrubCard(root, admin, p) {
  const scan = p.scan || {};
  const f = (name) => root.querySelector(`[data-f="${name}"]`);
  const scanRunning = scan.status === 'running' || scan.status === 'paused';
  const scanPct = Math.round(Number(scan.progressPct) || 0);

  // The scan chip leads the actions; both are keyed, so the chip keeps its
  // node while its percentage moves and the buttons keep theirs.
  const actions = root.querySelector('[data-part="scrub-actions"]');
  patchKeyedList(actions, [
    ...(scanRunning ? [{ key: 'scan-chip', html: '<tf-chip></tf-chip>' }] : []),
    ...(admin ? scrubButtons(scan.status) : []),
  ]);
  if (scanRunning) {
    const chip = actions.firstElementChild;
    setAttr(chip, 'status', scan.status === 'paused' ? 'warn' : 'accent');
    setAttr(chip, 'label', T('pools.scan_' + scan.kind, { pct: scanPct }));
  }
  const bar = slotEl(root.querySelector('[data-slot="scan-bar"]'), scanRunning, 'bar', '<tf-progress-bar tone="accent"></tf-progress-bar>');
  if (bar) {
    setAttr(bar, 'value', String(scanPct));
    setAttr(bar, 'label', T('pool.scan_eta', { eta: fmtDuration(scan.etaSecs), scanned: fmtBytes(scan.scannedBytes) }));
  }

  const finished = Boolean(p.lastScrubAt) && scan.status === 'finished';
  setText(f('last-when'), p.lastScrubAt
    ? [fmtDate(p.lastScrubAt), finished && scan.durationSecs ? fmtDuration(scan.durationSecs) : ''].filter(Boolean).join(' · ')
    : T('pools.never'));
  setText(f('last-sep'), finished ? ' · ' : '');
  const lastErrs = f('last-errs');
  setText(lastErrs, finished ? T('pools.scrub_errors', { n: Number(scan.errors) || 0 }) : '');
  setClass(lastErrs, 'num-err', finished && Boolean(scan.errors));
  setClass(lastErrs, 'num-ok', finished && !scan.errors);

  setText(f('scrub-sched'), p.scrubSchedule ? fmtSchedule(p.scrubSchedule) : T('schedule.none'));
  setText(f('scrub-next'), p.nextScrubAt ? fmtIn(p.nextScrubAt) : '');
  const errors = f('pool-errors');
  setText(errors, `${Number(p.readErrors) || 0} / ${Number(p.writeErrors) || 0} / ${Number(p.cksumErrors) || 0}`);
  setClass(errors, 'num-err', Boolean(p.readErrors || p.writeErrors || p.cksumErrors));
  const autotrim = f('autotrim');
  setText(autotrim, T(p.autotrim ? 'schedule.on' : 'schedule.off'));
  setClass(autotrim, 'num-ok', Boolean(p.autotrim));

  // `zpool trim` (§5.10, research R7), next to the scrub it belongs beside:
  // both are the pool's own maintenance, both run on a clock. A pool whose
  // devices cannot TRIM says so instead of offering an action ZFS refuses.
  const trimState = String(p.trimState || 'idle');
  const trimSupported = trimState !== 'unsupported';
  const trimRunning = trimState === 'trimming' || trimState === 'suspended';
  const trimLabel = f('trim-label');
  setText(trimLabel, trimSupported ? T('pool.trim_state_' + trimState) : T('pool.trim_unsupported'));
  setClass(trimLabel, 'text-3', !trimSupported);
  setText(f('trim-pct'), trimSupported && trimRunning ? `${Math.round(Number(p.trimProgressPct) || 0)}%` : '');
  setText(f('trim-last'), trimSupported && p.lastTrimAt ? fmtDate(p.lastTrimAt) : '');
  patchKeyedList(root.querySelector('[data-part="trim-buttons"]'), admin && trimSupported ? trimButtons(trimState) : []);
  const trimPill = f('trim-pill');
  setAttr(trimPill, 'data-act', admin && trimSupported ? 'trim-schedule' : null);
  setAttr(trimPill, 'role', admin && trimSupported ? 'button' : null);
  setText(f('trim-sched'), p.trimSchedule ? fmtSchedule(p.trimSchedule) : T('schedule.none'));
  setText(f('trim-next'), p.nextTrimAt ? fmtIn(p.nextTrimAt) : '');
}

function paintTopology(screen, body, state, refresh) {
  const host = body.querySelector('#nas-pool-tab-body');
  const p = state.res.pool;
  const admin = screen.isAdmin;
  const free = state.freeDisks;
  let root = paneRoot(host, 'topology');
  const built = !root;
  if (built) root = buildPane(host, 'topology', topologySkeletonHtml(screen), screen, state, refresh);

  if (admin) {
    const freeNvme = free.some((d) => d.kind === 'nvme');
    for (const [role, enabled, reason] of [['data', free.length > 0, T('pools.no_free_disks')], ['cache', freeNvme, T('pool.no_free_nvme')], ['spare', free.length > 0, T('pools.no_free_disks')]]) {
      const b = root.querySelector(`[data-act="add-vdev"][data-role="${role}"]`);
      setAttr(b, 'disabled', !enabled);
      setAttr(b, 'title', enabled ? null : reason);
    }
  }

  // Vdev groups are keyed by id and their disk cells by leaf name: a poll
  // that moves a temperature, a counter or a health dot writes those values
  // into the nodes on screen, and only a vdev or disk that comes or goes
  // changes the structure — and then only its own group or cell.
  const vdevs = p.vdevs || [];
  const vdevHost = root.querySelector('[data-part="vdevs"]');
  patchKeyedList(vdevHost, vdevs.length
    ? vdevs.map((v) => ({ key: `vdev:${v.id}:${v.role}:${v.kind}`, html: vdevSkeletonHtml(v, admin, vdevs) }))
    : [{ key: 'none', html: `<div class="muted">${escapeHtml(T('pool.no_vdevs'))}</div>` }]);
  const scan = p.scan || {};
  const ctx = {
    pool: p, admin, free,
    inventory: inventoryOf(state.disks),
    resilvering: scan.kind === 'resilver' && scan.status === 'running',
  };
  const groups = new Map([...vdevHost.children].map((el) => [el.dataset.vdev, el]));
  for (const v of vdevs) paintVdevGroup(groups.get(v.id), v, ctx);

  paintScrubCard(root, admin, p);
  paintIoRows(root, p.io || {});
  const chart = root.querySelector('#nas-pool-io-live');
  if (built) {
    // A fresh pane seeds its chart from the samples kept on `state`: the
    // chart is new because the TAB was (re)opened, never because of a poll.
    mountIoChart(chart, 72, state);
  } else {
    // The chart already owns the points it was seeded with and every one
    // pushed since; feed it the newest sample so it keeps moving.
    const last = state.ioSamples[state.ioSamples.length - 1];
    if (last) chart.push(last.t, { read: last.read, write: last.write });
  }
  paintProperties(screen, root, state, refresh);
}

function openScrubScheduleEditor(screen, pool, refresh) {
  openScheduleEditor({
    title: T('pool.scrub_schedule_title', { name: pool.name }),
    icon: 'shield',
    schedule: pool.scrubSchedule || { every: 'weekly', hour: 2, minute: 0, weekday: 0, day: 1 },
    enabled: Boolean(pool.scrubSchedule),
    allowed: ['daily', 'weekly', 'monthly'],
    note: T('pool.scrub_schedule_note'),
    onSave: async ({ enabled, schedule }) => {
      await screen.nas('tentaNasScrubScheduleSetRequest', { name: pool.name, enabled, schedule });
      toast(T('schedule.saved'), 'success');
      refresh();
    },
  });
}

/// The recurring TRIM (§5.10). Monthly by default and never sub-daily: TRIM
/// competes with real I/O, and a pool that needs it more often than once a
/// month has a bigger problem than untrimmed blocks.
function openTrimScheduleEditor(screen, pool, refresh) {
  openScheduleEditor({
    title: T('pool.trim_schedule_title', { name: pool.name }),
    icon: 'zap',
    schedule: pool.trimSchedule || { every: 'monthly', hour: 3, minute: 30, weekday: 0, day: 1 },
    enabled: Boolean(pool.trimSchedule),
    allowed: ['weekly', 'monthly'],
    note: T('pool.trim_schedule_note'),
    onSave: async ({ enabled, schedule }) => {
      await screen.nas('tentaNasTrimScheduleSetRequest', { name: pool.name, enabled, schedule });
      toast(T('schedule.saved'), 'success');
      refresh();
    },
  });
}

// ---------------------------------------------------------------------------
// Stats (live throughput + 24h history)
// ---------------------------------------------------------------------------

function paintStats(body, state) {
  const host = body.querySelector('#nas-pool-tab-body');
  // ONE HOST, ONE WRITER: this direct write is legal only because `drawInner`
  // nulls `host.__tfHtml` on every tab switch and this is the first
  // synchronous statement here, before any await. Do not call `paintStats`
  // from `refresh` or a poll without switching it to `patchHtml` — the stale
  // cache would turn the next patch into a no-op and freeze the pane.
  host.innerHTML = `
    <div class="stack">
      <div class="section-card">
        <div class="chart-head"><div class="ch-title">${sprite('trend')} ${escapeHtml(T('pool.stats_live'))}</div><div class="ch-val" id="nas-pool-live-val"></div></div>
        <tf-stream-chart id="nas-pool-live"></tf-stream-chart>
        <div class="live-label"><span class="live-dot"></span>${escapeHtml(T('overview.live_window', { w: fmtDuration(IO_WINDOW_SECS) }))}</div>
      </div>
      <div class="grid-2">
        <div class="section-card">
          <div class="chart-head"><div class="ch-title">${sprite('line-chart')} ${escapeHtml(T('pool.stats_history_io'))}</div><div class="ch-val" id="nas-pool-hist-io-val"></div></div>
          <div id="nas-pool-hist-io"></div>
        </div>
        <div class="section-card">
          <div class="chart-head"><div class="ch-title">${sprite('clock')} ${escapeHtml(T('pool.stats_history_latency'))}</div><div class="ch-val" id="nas-pool-hist-lat-val"></div></div>
          <div id="nas-pool-hist-lat"></div>
        </div>
      </div>
    </div>`;
  state.live = mountIoChart(host.querySelector('#nas-pool-live'), 170, state);
  paintLiveVal(body, state);

  const samples = (state.res.history || [])
    .map((h) => ({ ...h, t: parseServerTs(h.at)?.getTime() }))
    .filter((h) => h.t != null)
    .sort((a, b) => a.t - b.t);
  const timeAxis = { scale: 'time', ticks: 6, format: (v) => new Intl.DateTimeFormat(I18n.getLanguage(), { hour: '2-digit', minute: '2-digit' }).format(new Date(v)) };
  const mount = (hostId, valId, series, yAxis, valueFormat, summary) => {
    const chartHost = host.querySelector('#' + hostId);
    const val = host.querySelector('#' + valId);
    if (series.flatMap((s) => s.points).length < 2) {
      chartHost.innerHTML = `<div class="muted">${escapeHtml(T('disk.history_empty'))}</div>`;
      return;
    }
    const chart = document.createElement('tf-line-chart');
    chart.height = 150;
    chart.legend = series.length > 1 ? { position: 'bottom', alignment: 'start' } : { position: 'none' };
    chart.xAxis = timeAxis;
    chart.yAxis = yAxis;
    chart.tooltip = { valueFormat };
    chart.narrow = null;
    chart.series = series;
    chartHost.replaceChildren(chart);
    val.textContent = summary;
  };
  const peak = samples.reduce((m, h) => Math.max(m, Number(h.readBps) || 0, Number(h.writeBps) || 0), 0);
  mount('nas-pool-hist-io', 'nas-pool-hist-io-val', [
    { id: 'read', name: T('disk.legend_read'), tone: 'primary', style: 'solid', showInLegend: true, points: samples.map((h) => ({ x: h.t, y: Number(h.readBps) || 0 })) },
    { id: 'write', name: T('disk.legend_write'), tone: 'info', style: 'solid', showInLegend: true, points: samples.map((h) => ({ x: h.t, y: Number(h.writeBps) || 0 })) },
  ], { min: 0, ticks: 4, format: (v) => fmtMBps(v) }, (v) => `${fmtMBps(v)} MB/s`, samples.length ? T('disk.peak', { v: fmtMBps(peak) }) : '');
  const lat = samples.filter((h) => h.awaitMs != null);
  mount('nas-pool-hist-lat', 'nas-pool-hist-lat-val', [
    { id: 'await', name: T('pool.io_latency'), tone: 'warning', style: 'solid', showInLegend: false, points: lat.map((h) => ({ x: h.t, y: Number(h.awaitMs) || 0 })) },
  ], { min: 0, ticks: 4, format: (v) => `${Math.round(v)}` }, (v) => `${v.toFixed(1)} ms`, lat.length ? T('pool.latency_max', { v: Math.max(...lat.map((h) => Number(h.awaitMs) || 0)).toFixed(1) }) : '');
}

// One ring of read/write samples per pool screen: n06 draws it both on the
// topology IO card and on the Statystyki tab, and switching inner tabs mounts
// a fresh chart, so the samples cannot live in either chart. (A POLL never
// re-mounts one — it only pushes the newest sample.)
function recordIoSample(state) {
  const io = state.res.pool.io || {};
  const now = Date.now();
  state.ioSamples.push({ t: now, read: Number(io.readBps) || 0, write: Number(io.writeBps) || 0 });
  // Keep one sample beyond the window so the line reaches the left edge.
  const keepFrom = now - (IO_WINDOW_SECS + 5) * 1000;
  while (state.ioSamples.length > 2 && state.ioSamples[1].t < keepFrom) state.ioSamples.shift();
}

function mountIoChart(chart, height, state) {
  chart.height = height;
  chart.window = IO_WINDOW_SECS;
  chart.legend = { position: 'none' };
  chart.tooltip = { valueFormat: (v) => `${fmtMBps(v)} MB/s` };
  // The n06 IO card is a 72 px strip — four Y ticks would not fit in it.
  chart.yAxis = { min: 0, ticks: height > 100 ? 4 : 2, format: (v) => fmtMBps(v) };
  chart.series = [
    { id: 'read', name: T('disk.legend_read'), tone: 'primary', style: 'solid', showInLegend: false, points: state.ioSamples.map((s) => ({ x: s.t, y: s.read })) },
    { id: 'write', name: T('disk.legend_write'), tone: 'info', style: 'solid', showInLegend: false, points: state.ioSamples.map((s) => ({ x: s.t, y: s.write })) },
  ];
  return chart;
}

function paintLiveVal(body, state) {
  const last = state.ioSamples[state.ioSamples.length - 1];
  const val = body.querySelector('#nas-pool-live-val');
  if (!val || !last) return;
  // Swatches once, numbers as text — the same construct the overview uses in
  // `pushOverviewSamples`. The readout under a live chart is written on every
  // sample, so rebuilding it is a visible flicker right next to the motion.
  patchHtml(val, '<span class="sw primary"></span><span class="v-read"></span><span class="sw info"></span><span class="v-write"></span>');
  setText(val.querySelector('.v-read'), `${T('disk.legend_read')} ${fmtMBps(last.read)} MB/s  `);
  setText(val.querySelector('.v-write'), `${T('disk.legend_write')} ${fmtMBps(last.write)} MB/s`);
}

function pushLiveSample(body, state) {
  const live = state.live;
  const last = state.ioSamples[state.ioSamples.length - 1];
  if (live && live.isConnected && last) live.push(last.t, { read: last.read, write: last.write });
  paintLiveVal(body, state);
}

// ---------------------------------------------------------------------------
// Properties + danger zone
// ---------------------------------------------------------------------------

export const sourceChipHtml = (source) => `<tf-chip size="sm" status="${source === 'local' ? 'accent' : 'info'}" label="${escapeAttr(T('props.source_' + (source || 'default')))}"></tf-chip>`;

// n06 shows "Właściwości puli" + "Strefa niebezpieczna" at the foot of the
// topology pane AND keeps a dedicated Właściwości tab for them, so the two
// panes emit the same skeleton from here and share `paintProperties`. Nothing
// in it depends on a poll: the rows and the destroy row's list of datasets
// are written by `paintProperties` into the nodes this builds once.
function propertiesSectionHtml(screen) {
  const name = screen.pool;
  return `
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('settings')} ${escapeHtml(T('props.title'))}</div><span class="hint">${escapeHtml(T('props.hint'))}</span></div>
      <tf-table id="nas-pool-props" empty-message="${escapeAttr(T('props.none'))}">
        <tf-column key="name" label="${escapeAttr(T('props.col_name'))}" renderer="html" width="260"></tf-column>
        <tf-column key="value" label="${escapeAttr(T('props.col_value'))}" renderer="html" fill></tf-column>
      </tf-table>
    </div>
    ${screen.isAdmin ? `
    <div class="section-card danger-zone">
      <h4>${sprite('alert')} ${escapeHtml(T('danger.title'))}</h4>
      ${dangerRowHtml({ title: T('danger.export'), desc: T('danger.export_desc'), action: T('danger.export_action'), icon: 'arrow-out', act: 'export' })}
      <tf-checkbox id="nas-export-force" label="${escapeAttr(T('danger.export_force'))}"></tf-checkbox>
      ${dangerRowHtml({ title: T('danger.destroy', { name }), desc: '', action: T('danger.destroy_action'), icon: 'trash', act: 'destroy' })}
    </div>` : ''}`;
}

function paintPropertiesTab(screen, body, state, refresh) {
  const host = body.querySelector('#nas-pool-tab-body');
  const root = paneRoot(host, 'properties') || buildPane(host, 'properties', propertiesSectionHtml(screen), screen, state, refresh);
  paintProperties(screen, root, state, refresh);
}

// Fills the "Właściwości puli" table and the dataset list of the destroy row
// on the topology and properties panes (n06). Its buttons are handled by the
// pane's delegated listener (`onPaneClick`), so there is nothing to wire here.
function paintProperties(screen, host, state, refresh) {
  const p = state.res.pool;
  const admin = screen.isAdmin;
  const table = host.querySelector('#nas-pool-props');
  const props = state.res.properties || [];
  const editable = (name) => admin && (name in POOL_PROPS || name in DATASET_PROPS);
  // Assigned once: `set rowActions` runs a full table render, and the handler
  // needs only the row it is handed plus the pool name, which cannot change.
  if (!table.rowActions) table.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    if (!editable(row._prop.name)) return null;
    const wrap = document.createElement('div');
    wrap.innerHTML = `<tf-button size="sm" variant="ghost" icon="edit" data-act="edit" title="${escapeAttr(I18n.t('common.edit'))}"></tf-button>`;
    wrap.querySelector('[data-act="edit"]').addEventListener('click', (e) => { e.stopPropagation(); openPropertyEditor(screen, p.name, live()._prop, refresh); });
    return wrap;
  };
  const rows = props.map((pr) => ({
    _prop: pr,
    name: `<span class="tf-table__cell--mono">${escapeHtml(pr.name)}</span>`,
    value: `<span class="tf-table__cell--mono">${escapeHtml(pr.value ?? '—')}</span>${pr.name === 'compression' && p.compressRatio ? ` <tf-chip size="sm" status="ok" label="${escapeAttr(T('pool.ratio_chip', { ratio: fmtRatio(p.compressRatio) }))}"></tf-chip>` : ''}${pr.inheritedFrom ? `<div class="tf-table__cell-sub">${escapeHtml(T('props.inherited_from', { from: pr.inheritedFrom }))}</div>` : ''}`,
  }));
  // `rows =` is a full table render (M10): every cell, every row-action
  // button and any hover on them is rebuilt. The properties of a pool change
  // when someone edits one, not on a 5 s poll, so the assignment happens only
  // when the rows really differ from what the table was last given.
  const sig = JSON.stringify(rows);
  if (table.__tfRows !== sig) {
    table.__tfRows = sig;
    table.rows = rows;
  }

  const destroyRow = host.querySelector('[data-act="destroy"]')?.closest('.dz-row');
  if (destroyRow) {
    const childNames = (state.res.datasets || []).filter((d) => d.name !== p.name).map((d) => d.name.slice(p.name.length + 1)).join(', ') || '—';
    setText(destroyRow.querySelector('.dz-desc'), T('danger.destroy_desc', { names: childNames }));
  }
}

// One property at a time: a select for enumerated values, a text field for
// free ones, and "inherit" for dataset properties so a local override can
// be dropped instead of overwritten.
export function openPropertyEditor(screen, target, prop, onDone, { dataset = false } = {}) {
  const isDataset = dataset || prop.name in DATASET_PROPS;
  const values = isDataset ? DATASET_PROPS[prop.name] : POOL_PROPS[prop.name];
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('props.edit_title', { name: prop.name }));
  win.setAttribute('subtitle', target);
  win.setAttribute('icon', 'edit');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '480');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      ${values
        ? `<tf-select id="nas-prop-value" label="${escapeAttr(T('props.col_value'))}"></tf-select>`
        : `<tf-input id="nas-prop-value" label="${escapeAttr(T('props.col_value'))}" value="${escapeAttr(prop.value ?? '')}" autocomplete="off"></tf-input>`}
      ${isDataset ? `<tf-checkbox id="nas-prop-inherit" label="${escapeAttr(T('props.inherit'))}"></tf-checkbox>` : ''}
      <div class="muted">${escapeHtml(T('props.current', { value: prop.value ?? '—', source: T('props.source_' + (prop.source || 'default')) }))}</div>
      <div class="num-err" id="nas-prop-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="save" data-action="confirm">${escapeHtml(I18n.t('common.save'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const valueEl = win.querySelector('#nas-prop-value');
  if (values) {
    const list = values.includes(prop.value) ? values : [prop.value, ...values].filter((v) => v != null && v !== '');
    valueEl.setOptions(list.map((v) => ({ value: v, label: v })), prop.value);
  }
  let busy = false;
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    busy = true;
    const inherit = Boolean(win.querySelector('#nas-prop-inherit')?.checked);
    const change = { name: prop.name, value: inherit ? '' : String(valueEl.value ?? ''), inherit };
    try {
      const kind = isDataset ? 'tentaNasDatasetSetPropertiesRequest' : 'tentaNasPoolSetPropertiesRequest';
      const res = await screen.withSudo((sudoPassword) => screen.nas(kind, { name: target, changes: [change], sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('props.edit_title', { name: prop.name }));
      if (res === null) { busy = false; return; }
      toast(T('props.saved', { name: prop.name }), 'success');
      win.close(true);
      if (onDone) onDone(res);
    } catch (err) {
      busy = false;
      const errEl = win.querySelector('#nas-prop-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  return win;
}

// Destroy (n17a): the loss list names every dataset that goes with the pool
// and the disks that come back as free; the name must be retyped.
export function openPoolDestroyDialog(screen, pool, datasets, onDone) {
  // The pool root is the subject of the modal, not an item in it: listing it
  // would show the pool's whole capacity twice (root + its children) and shift
  // the "+N więcej" overflow counter by one.
  const children = datasets.filter((d) => d.name !== pool.name);
  const shown = children.slice(0, 8);
  const more = children.length - shown.length;
  const dataVdevs = (pool.vdevs || []).filter((v) => v.role === 'data');
  const dataDisks = dataVdevs.flatMap((v) => v.disks || []);
  const otherRoles = [...new Set((pool.vdevs || []).filter((v) => v.role !== 'data').map((v) => v.role))].map((r) => T('pool.role_' + r));
  const explain = T('destroy_pool.explain', {
    n: dataDisks.length,
    layout: escapeHtml(layoutLabel(dataVdevs[0]?.kind || pool.layout)),
    disks: escapeHtml(dataDisks.map((d) => d.name).join(', ') || '—'),
    others: otherRoles.length ? escapeHtml(T('destroy_pool.explain_others', { roles: otherRoles.join(', ') })) : '',
    export: `<b>${escapeHtml(T('danger.export'))}</b>`,
  });
  const bodyHtml = `
    <div class="wizard-warning danger">${sprite('alert')}<div>${T('destroy_pool.warning', { name: escapeHtml(pool.name) })}</div></div>
    <ul class="loss-list">
      ${shown.map((d) => `<li class="ll bad">${sprite('trash')}<span><b>${escapeHtml(d.name)}</b> — ${escapeHtml(fmtBytes(d.usedBytes))}</span></li>`).join('')}
      ${more > 0 ? `<li class="ll bad">${sprite('trash')}<span>${escapeHtml(T('destroy_pool.more', { n: more }))}</span></li>` : ''}
      <li class="ll bad">${sprite('trash')}<span><b>${escapeHtml(T('destroy_pool.snapshots', { n: pool.snapshotCount }))}</b></span></li>
    </ul>
    <div class="explain-box">${explain}</div>`;
  return openRetypeDialog({
    title: T('destroy_pool.title', { name: pool.name }),
    icon: 'alert',
    name: pool.name,
    bodyHtml,
    retypeLabel: `${escapeHtml(T('destroy_pool.retype'))} <span class="mono num-err">${escapeHtml(pool.name)}</span>`,
    confirmLabel: T('destroy_pool.confirm', { name: pool.name }),
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolDestroyRequest', { name: pool.name, confirmName: pool.name, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('destroy_pool.title', { name: pool.name }));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('destroy_pool.done', { name: pool.name }));
      return true;
    },
  });
}

// ---------------------------------------------------------------------------
// Vdev dialogs
// ---------------------------------------------------------------------------

// Layouts a vdev of `n` disks can take for a role. Cache and spares are
// always single devices; a log or special vdev should be mirrored.
function vdevLayouts(role, n) {
  if (role === 'cache' || role === 'spare') return n >= 1 ? ['stripe'] : [];
  const out = [];
  if (n >= 1) out.push('stripe');
  if (n >= 2) out.push('mirror');
  if (role === 'data') {
    if (n >= 3) out.push('raidz1');
    if (n >= 4) out.push('raidz2');
    if (n >= 5) out.push('raidz3');
  }
  return out;
}

export function openAddVdevDialog(screen, pool, initialRole, freeDisks, onDone) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('add_vdev.title', { name: pool.name }));
  win.setAttribute('icon', 'plus');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '680');
  win.setAttribute('min-width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const state = { role: initialRole || 'data', diskIds: new Set(), layout: '', busy: false };
  const roles = ['data', 'cache', 'log', 'spare', 'special'];
  // A log or special vdev only pays for itself on flash: a SLOG exists to cut
  // write latency and a special vdev to serve metadata, and a spinning disk
  // does neither. `pool.hint_special` already promises "metadane + małe bloki
  // na NVMe" — nothing enforced it, so the dialog would happily build either
  // out of HDDs, and neither can be removed from a raidz pool afterwards.
  // Only 'hdd' is refused, never 'unknown': a disk whose media the inventory
  // could not read may well be flash, and refusing it would be a guess.
  const FLASH_ROLES = new Set(['log', 'special']);

  win.innerHTML = `
    <div slot="body" class="stack">
      <tf-select id="nas-av-role" label="${escapeAttr(T('add_vdev.role'))}"></tf-select>
      <div class="explain-box" id="nas-av-explain"></div>
      <h2 class="wizard-section-title">${escapeHtml(T('wizard_pool.disks_title'))}</h2>
      <div class="disk-cells" id="nas-av-disks">${freeDisks.map((d) => `
        <div class="disk-cell" data-disk="${escapeAttr(d.diskId)}">
          <tf-checkbox></tf-checkbox>
          <div class="dc-main">
            <div class="dc-name"><span class="health-dot ${healthClass(d.health)}"></span><span class="mono">${escapeHtml(d.name)}</span><span class="disk-kind ${escapeAttr(d.kind)}">${escapeHtml(d.kind)}</span></div>
            <div class="dc-sub">${escapeHtml([fmtBytes(d.sizeBytes), d.model || ''].filter(Boolean).join(' · '))}</div>
          </div>
        </div>`).join('')}</div>
      <tf-select id="nas-av-layout" label="${escapeAttr(T('add_vdev.layout'))}"></tf-select>
      ${warningHtml('danger', T('wizard_pool.erase_warning_none'))}
      <div class="num-err" id="nas-av-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="plus" data-action="confirm" disabled>${escapeHtml(T('add_vdev.confirm'))}</tf-button>
    </div>`;
  document.body.appendChild(win);

  const roleSel = win.querySelector('#nas-av-role');
  const layoutSel = win.querySelector('#nas-av-layout');
  const btn = win.querySelector('[data-action="confirm"]');
  roleSel.setOptions(roles.map((r) => ({ value: r, label: T('pool.role_' + r) })), state.role);
  const sync = () => {
    win.querySelector('#nas-av-explain').textContent = T('add_vdev.explain_' + state.role);
    const layouts = vdevLayouts(state.role, state.diskIds.size);
    if (!layouts.includes(state.layout)) state.layout = layouts[layouts.length - 1] || '';
    layoutSel.setOptions(layouts.map((l) => ({ value: l, label: layoutLabel(l) })), state.layout);
    const spinning = FLASH_ROLES.has(state.role)
      ? freeDisks.filter((d) => state.diskIds.has(d.diskId) && d.kind === 'hdd')
      : [];
    const err = win.querySelector('#nas-av-error');
    err.textContent = spinning.length
      ? T('add_vdev.flash_only', { role: T('pool.role_' + state.role), disks: spinning.map((d) => d.name).join(', ') })
      : '';
    err.hidden = spinning.length === 0;
    if (state.diskIds.size && state.layout && !state.busy && !spinning.length) btn.removeAttribute('disabled');
    else btn.setAttribute('disabled', '');
  };
  roleSel.addEventListener('change', (e) => { state.role = e.detail.value; sync(); });
  layoutSel.addEventListener('change', (e) => { state.layout = e.detail.value; sync(); });
  const cells = win.querySelector('#nas-av-disks');
  cells.addEventListener('click', toggleCellCheckbox);
  cells.addEventListener('change', (e) => {
    const cell = e.target.closest('.disk-cell[data-disk]');
    if (!cell) return;
    const on = Boolean(e.detail?.checked);
    if (on) state.diskIds.add(cell.dataset.disk); else state.diskIds.delete(cell.dataset.disk);
    cell.classList.toggle('checked', on);
    sync();
  });
  sync();

  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (state.busy || !state.diskIds.size || !state.layout) return;
    state.busy = true;
    sync();
    const payload = { name: pool.name, role: state.role, layout: state.layout, diskIds: [...state.diskIds] };
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolAddVdevRequest', { ...payload, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('add_vdev.title', { name: pool.name }));
    state.busy = false;
    if (res === null) { sync(); return; }
    win.close(true);
    followResponse(screen, res, onDone, T('add_vdev.done'));
  });
  return win;
}

// Single-disk picker shared by "expand vdev" and the replace wizard step:
// disks smaller than `minBytes` stay visible but cannot be picked.
export function openPickDiskDialog(screen, { title, explain, disks, minBytes = 0, confirmLabel, onPick }) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', title);
  win.setAttribute('icon', 'cylinder');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '600');
  win.setAttribute('min-width', '480');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="explain-box">${escapeHtml(explain)}</div>
      <div id="nas-pick-list">${disks.length ? disks.map((d) => {
        const small = (Number(d.sizeBytes) || 0) < minBytes;
        return `<tf-option-row value="${escapeAttr(d.diskId)}" label="${escapeAttr(d.name)}" sub="${escapeAttr([fmtBytes(d.sizeBytes), d.model || '', small ? T('pool.disk_too_small', { min: fmtBytes(minBytes) }) : ''].filter(Boolean).join(' · '))}" ${small ? 'disabled' : ''}></tf-option-row>`;
      }).join('') : `<div class="muted">${escapeHtml(T('pools.no_free_disks'))}</div>`}</div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="check" data-action="confirm" disabled>${escapeHtml(confirmLabel)}</tf-button>
    </div>`;
  document.body.appendChild(win);
  let picked = null;
  let busy = false;
  const btn = win.querySelector('[data-action="confirm"]');
  win.querySelector('#nas-pick-list').addEventListener('option-select', (e) => {
    picked = disks.find((d) => d.diskId === e.detail.value) || null;
    win.querySelectorAll('tf-option-row').forEach((r) => { r.selected = r.getAttribute('value') === e.detail.value; });
    if (picked) btn.removeAttribute('disabled');
  });
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy || !picked) return;
    busy = true;
    btn.setAttribute('disabled', '');
    const done = await onPick(picked);
    if (done === false) { busy = false; btn.removeAttribute('disabled'); return; }
    win.close(true);
  });
  return win;
}

const resilverActive = (scan) => scan?.kind === 'resilver' && (scan.status === 'running' || scan.status === 'paused');

// Replace (n17c): pick the replacement (a hot-spare of the pool first, then
// the free disks) → the replace job → the resilver followed from `zpool
// status` until the vdev is whole again. The install-wizard shell keeps it
// consistent with pool creation.
//
// The window is built once. A step change swaps the step body (a structural
// change, once per step); every 1.5 s tick only writes the rail classes, the
// progress bar's attributes and the log's new tail into the nodes on screen.
export function openReplaceWizard(screen, { pool, vdev, disk, freeDisks, disks = [], onDone }) {
  if (screen.openWindow) { screen.openWindow.remove(); screen.openWindow = null; }
  // The disk being replaced is exactly the one M4 flags: a leaf `zpool
  // status` no longer finds prints as its numeric GUID or, if its by-id
  // symlink is gone, as that by-id basename. Every visible spot below uses
  // this label instead of `disk.name`; the wire request still sends
  // `disk.name` (the GUID/by-id text), which is what the server needs to
  // find it.
  const oldDiskLabel = leafDisplayName(disk, inventoryFor(inventoryOf(disks), disk));
  const minBytes = Number(disk.sizeBytes) || 0;
  const byId = new Map(disks.map((d) => [d.diskId, d]));
  const candidates = [
    ...(pool.vdevs || []).filter((v) => v.role === 'spare').flatMap((v) => v.disks || []).filter((s) => s.state === 'online').map((s) => ({ ...byId.get(s.diskId), ...s, spare: true })),
    ...freeDisks.map((d) => ({ ...d, spare: false })),
  ].map((d) => ({ ...d, small: (Number(d.sizeBytes) || 0) < minBytes }));
  const state = { step: 0, pick: null, job: null, scan: null, result: null, timer: null };
  const steps = [T('replace.step_pick'), T('replace.step_run'), T('replace.step_resilver')];
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('replace.title', { device: oldDiskLabel, pool: pool.name, layout: layoutLabel(vdev.kind) }));
  win.setAttribute('icon', 'refresh');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '820');
  win.setAttribute('min-width', '640');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  screen.openWindow = win;

  const optionHtml = (d) => {
    const name = d.spare
      ? `${escapeHtml(T('replace.spare_name', { name: d.name, pool: pool.name }))} <tf-chip size="sm" status="ok" dot label="${escapeAttr(T('replace.spare_ready'))}"></tf-chip>`
      : escapeHtml(T('replace.free_name', { name: d.name, size: fmtBytes(d.sizeBytes), model: d.model || '—' }));
    const sub = d.spare
      ? T('replace.spare_sub', { size: fmtBytes(d.sizeBytes), model: d.model || '—', serial: d.serial || '—' })
      : T('replace.free_sub', { serial: d.serial || '—' });
    return `
      <div class="target-option ${d.small ? 'disabled' : ''}" data-disk="${escapeAttr(d.diskId)}" ${d.small ? `title="${escapeAttr(T('pool.disk_too_small', { min: fmtBytes(minBytes) }))}"` : ''}>
        <tf-checkbox ${d.small ? 'disabled' : ''}></tf-checkbox>
        <div class="t-body">
          <div class="t-name">${name}</div>
          <div class="t-sub">${escapeHtml(sub)}${d.small ? ` · ${escapeHtml(T('pool.disk_too_small', { min: fmtBytes(minBytes) }))}` : ''}</div>
        </div>
      </div>`;
  };

  const explainHtml = () => (state.pick
    ? T('replace.explain', { old: escapeHtml(oldDiskLabel), new: escapeHtml(state.pick.name), pool: escapeHtml(pool.name), layout: escapeHtml(layoutLabel(vdev.kind)), ft: Math.max(0, (Number(vdev.faultTolerance) || 0) - 1) })
    : escapeHtml(T('replace.explain_pick')));

  // One skeleton per phase. What moves inside a phase (bar value and label,
  // log tail) is left empty here and written by `paint`.
  const phaseHtml = (phase) => {
    switch (phase) {
      case 'pick': return `
        <div class="stack" id="nas-rp-list">${candidates.map(optionHtml).join('') || `<div class="muted">${escapeHtml(T('pools.no_free_disks'))}</div>`}</div>
        <div class="explain-box mt-md" id="nas-rp-explain">${explainHtml()}</div>
        <div class="wizard-warning mt-md">${sprite('alert')}<div>${escapeHtml(T('replace.warning', { device: oldDiskLabel }))}</div></div>`;
      case 'run': return `
        <h2 class="wizard-section-title">${escapeHtml(T('replace.run_title', { old: oldDiskLabel, new: state.pick.name }))}</h2>
        <p class="wizard-section-sub">${escapeHtml(T('replace.sub_run'))}</p>
        <div data-slot="bar"></div>
        <pre class="job-log mono mt-sm"></pre>`;
      case 'resilver': return `
        <h2 class="wizard-section-title">${escapeHtml(T('replace.step_resilver'))}</h2>
        <p class="wizard-section-sub">${escapeHtml(T('replace.sub_resilver'))}</p>
        <tf-progress-bar tone="accent"></tf-progress-bar>
        ${warningHtml('info', T('replace.warning', { device: oldDiskLabel }))}
        <pre class="job-log mono mt-sm"></pre>`;
      default: {
        const ok = state.result.ok;
        return `<div class="result-box ${ok ? 'ok' : 'err'}">${sprite(ok ? 'check-circle' : 'alert')}<h3>${escapeHtml(ok ? T('replace.done_title') : T('replace.failed_title'))}</h3><p>${escapeHtml(state.result.detail || '')}</p></div><pre class="job-log mono mt-sm"></pre>`;
      }
    }
  };

  // n17c shows the step rail alone in the window body — the window title
  // already names the disk, the pool and its layout.
  win.innerHTML = `
    <div slot="body">
      <div class="install-progress">${steps.map((s, i) => `<div class="install-step" data-step="${i}"><span class="num"></span><span class="label">${escapeHtml(s)}</span></div>`).join('')}</div>
      <div class="install-step-body"></div>
    </div>
    <div slot="footer"></div>`;
  const stepBody = win.querySelector('.install-step-body');
  const footer = win.querySelector('[slot="footer"]');

  const paint = () => {
    win.querySelectorAll('.install-progress .install-step').forEach((el, i) => {
      setClass(el, 'active', i === state.step);
      setClass(el, 'done', i < state.step);
      const num = el.querySelector('.num');
      const want = i < state.step ? 'done' : String(i + 1);
      if (num.dataset.v !== want) {
        num.dataset.v = want;
        num.innerHTML = i < state.step ? sprite('check') : String(i + 1);
      }
    });

    const phase = state.result ? `result-${state.result.ok ? 'ok' : 'err'}` : ['pick', 'run', 'resilver'][state.step];
    if (stepBody.dataset.phase !== phase) {
      stepBody.dataset.phase = phase;
      stepBody.innerHTML = phaseHtml(phase);
      if (phase === 'pick') wirePickList();
    }
    if (phase === 'run') {
      const bar = slotEl(stepBody.querySelector('[data-slot="bar"]'), Boolean(state.job), 'bar', '<tf-progress-bar tone="accent"></tf-progress-bar>');
      if (bar) {
        setAttr(bar, 'value', String(Number(state.job.progressPct) || 0));
        setAttr(bar, 'label', T('jobs.status_' + state.job.status));
      }
    } else if (phase === 'resilver') {
      const scan = state.scan || {};
      const pctDone = Math.round(Number(scan.progressPct) || 0);
      const bar = stepBody.querySelector('tf-progress-bar');
      setAttr(bar, 'value', String(pctDone));
      setAttr(bar, 'label', T('replace.resilver_progress', { pct: pctDone, eta: fmtDuration(scan.etaSecs) }));
    }
    const log = stepBody.querySelector('.job-log');
    if (log) {
      setAttr(log, 'hidden', !state.job);
      paintJobLog(log, state.job?.log);
    }

    // Footer buttons are keyed: "Anuluj" and "Wstecz" are the same nodes for
    // the window's whole life, only the forward button changes with the phase.
    const running = state.step > 0 && !state.result;
    patchKeyedList(footer, [
      { key: 'cancel', html: `<tf-button variant="ghost" data-wizard-cancel>${escapeHtml(I18n.t('common.cancel'))}</tf-button>` },
      { key: 'back', html: `<tf-button variant="ghost" icon="chevron-left" data-wizard-back disabled>${escapeHtml(I18n.t('common.back'))}</tf-button>` },
      { key: 'spacer', html: '<span class="spacer"></span>' },
      ...(state.result
        ? [{ key: 'close', html: `<tf-button variant="primary" icon="check" data-wizard-next>${escapeHtml(I18n.t('common.close'))}</tf-button>` }]
        : state.step === 0
          ? [{ key: 'start', html: `<tf-button variant="primary" icon="play" data-wizard-next>${escapeHtml(T('replace.start'))}</tf-button>` }]
          : []),
    ]);
    setAttr(footer.querySelector('[data-wizard-cancel]'), 'disabled', running);
    if (!state.result && state.step === 0) setAttr(footer.querySelector('[data-wizard-next]'), 'disabled', !state.pick);
  };

  function wirePickList() {
    const list = stepBody.querySelector('#nas-rp-list');
    list.addEventListener('click', (e) => {
      const opt = e.target.closest('.target-option[data-disk]');
      if (!opt || opt.classList.contains('disabled') || e.target.closest('tf-checkbox')) return;
      pick(opt.dataset.disk);
    });
    list.addEventListener('change', (e) => {
      const opt = e.target.closest('.target-option[data-disk]');
      if (!opt || opt.classList.contains('disabled')) return;
      pick(opt.dataset.disk);
    });
  }

  footer.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-wizard-cancel], [data-wizard-next]');
    if (!btn || btn.hasAttribute('disabled')) return;
    if (btn.hasAttribute('data-wizard-cancel')) win.close();
    else next();
  });

  // One replacement at a time: picking an option unchecks the others.
  const pick = (diskId) => {
    state.pick = candidates.find((d) => d.diskId === diskId && !d.small) || null;
    win.querySelectorAll('.target-option[data-disk]').forEach((o) => {
      const on = state.pick && o.dataset.disk === state.pick.diskId;
      o.classList.toggle('checked', Boolean(on));
      o.querySelector('tf-checkbox').checked = Boolean(on);
    });
    win.querySelector('#nas-rp-explain').innerHTML = explainHtml();
    paint();
  };

  const next = async () => {
    if (state.result) { win.close(); return; }
    if (state.step !== 0 || !state.pick) return;
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolReplaceDiskRequest', { name: pool.name, old: disk.name, diskId: state.pick.diskId, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('replace.title', { device: oldDiskLabel, pool: pool.name, layout: layoutLabel(vdev.kind) }));
    if (!res) return;
    state.step = 1;
    state.job = res.job || null;
    paint();
    if (state.job) await pollJob(); else await pollResilver();
  };

  // The replace job spans `zpool replace` AND the resilver it starts (the
  // core follows the scan before it reports the job done), so waiting for the
  // job to finish before looking at the scan left step 3 unreachable for the
  // whole resilver. Each tick therefore also reads the pool's scan: once a
  // resilver runs, the rail moves to step 3 and shows its progress and ETA
  // while the job's log keeps growing underneath. A failed PoolGet here is
  // not the job's failure — the job stays the authority, the rail just waits.
  const pollJob = async () => {
    if (!win.isConnected || !state.job) return;
    try {
      const r = await screen.nas('tentaNasJobGetRequest', { jobId: state.job.jobId });
      state.job = r.job;
    } catch (e) {
      state.step = 2;
      state.result = { ok: false, detail: errMessage(e) };
      paint();
      // A failed job left the pool page showing the pre-replace topology
      // until its next poll (MINOR 7) — refresh it the same way a
      // successful replace does.
      if (onDone) onDone(state.job);
      return;
    }
    if (!win.isConnected) return;
    const s = state.job.status;
    if (s === 'running' || s === 'queued') {
      try {
        const r = await screen.nas('tentaNasPoolGetRequest', { name: pool.name });
        const scan = r.pool?.scan || null;
        if (resilverActive(scan)) {
          state.scan = scan;
          state.step = 2;
        }
      } catch { /* the job decides; the rail only waits for the next tick */ }
      if (!win.isConnected) return;
      paint();
      state.timer = setTimeout(pollJob, POLL_JOB_MODAL_MS);
      return;
    }
    if (s !== 'succeeded' && s !== 'done') {
      state.step = 2;
      state.result = { ok: false, detail: state.job.error || T('jobs.status_' + s) };
      paint();
      if (onDone) onDone(state.job);
      return;
    }
    state.step = 2;
    paint();
    await pollResilver();
  };

  // After the job, `PoolGet` is read until the scan is no longer a running
  // resilver (a job without a follower, or one that returned early).
  const pollResilver = async () => {
    if (!win.isConnected) return;
    try {
      const r = await screen.nas('tentaNasPoolGetRequest', { name: pool.name });
      state.scan = r.pool?.scan || {};
    } catch (e) {
      state.result = { ok: false, detail: errMessage(e) };
      paint();
      if (onDone) onDone(state.job);
      return;
    }
    if (!win.isConnected) return;
    if (resilverActive(state.scan)) {
      paint();
      state.timer = setTimeout(pollResilver, POLL_JOB_MODAL_MS);
      return;
    }
    const failed = state.scan.kind === 'resilver' && Number(state.scan.errors) > 0;
    state.result = failed
      ? { ok: false, detail: T('pools.scrub_errors', { n: Number(state.scan.errors) || 0 }) }
      : { ok: true, detail: T('replace.done_detail', { device: oldDiskLabel }) };
    paint();
    if (onDone) onDone(state.job);
  };

  win.addEventListener('close-request', () => {
    if (state.timer) clearTimeout(state.timer);
    if (screen.openWindow === win) screen.openWindow = null;
  });
  paint();
  document.body.appendChild(win);
  return win;
}
