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
  layoutLabel, stateTone, stateLabel, stateChipHtml, fmtSchedule,
} from '/js/modules/tentanas/format.js';
import { openScheduleEditor } from '/js/modules/tentanas/schedule-editor.js';
import { openRetypeDialog, followResponse, dangerRowHtml, warningHtml } from '/js/modules/tentanas/dialogs.js';
import { scrubAction } from '/js/modules/tentanas/pools.js';
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
    paintKpis(body, state);
    if (screen.poolTab === 'topology') paintTopology(screen, body, state, refresh);
    if (screen.poolTab === 'properties') paintPropertiesTab(screen, body, state, refresh);
    if (screen.poolTab === 'stats') pushLiveSample(body, state);
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
  const diskWarnings = (p.vdevs || []).flatMap((v) => v.disks || []).filter((d) => d.state !== 'online' || (Number(d.readErrors) || 0) + (Number(d.writeErrors) || 0) + (Number(d.cksumErrors) || 0) > 0).length;
  const frag = Math.round(Number(p.fragmentationPct) || 0);
  body.querySelector('#nas-pool-kpi').innerHTML = `
    <tf-stat-card label="${escapeAttr(T('pool.kpi_capacity'))}" value="${escapeAttr(cap.value)}" suffix="${escapeAttr(cap.suffix)}" icon="database" ${usedPct > 90 ? 'accent="danger"' : usedPct > 75 ? 'accent="warning"' : ''} delta="${escapeAttr(T('pool.kpi_capacity_delta', { pct: usedPct, ratio: fmtRatio(p.compressRatio) }))}"></tf-stat-card>
    <tf-stat-card label="${escapeAttr(T('pool.kpi_state'))}" value="${escapeAttr(stateLabel(p.state).toUpperCase())}" icon="check" accent="${stateTone(p.state) === 'ok' ? 'success' : stateTone(p.state) === 'warn' ? 'warning' : 'danger'}" delta="${escapeAttr(T('pool.kpi_state_delta', { e: Number(scan.errors) || 0, w: diskWarnings }))}"></tf-stat-card>
    <tf-stat-card label="${escapeAttr(T('pool.kpi_iops'))}" value="${Math.round((Number(io.readIops) || 0) + (Number(io.writeIops) || 0))}" icon="zap" delta="${escapeAttr(T('pool.kpi_iops_delta', { r: Math.round(Number(io.readIops) || 0), w: Math.round(Number(io.writeIops) || 0) }))}"></tf-stat-card>
    <tf-stat-card label="${escapeAttr(T('pool.kpi_fragmentation'))}" value="${frag}" suffix="%" icon="grid-2x2" ${frag > 50 ? 'accent="warning"' : ''} delta="${escapeAttr(frag > 50 ? T('pool.frag_high') : T('pool.frag_low'))}" delta-type="${frag > 50 ? 'warn' : 'neutral'}"></tf-stat-card>`;

  const tabs = body.querySelector('#nas-pool-tabs');
  tabs.querySelector('#datasets')?.setAttribute('count', String(p.datasetCount ?? 0));
  tabs.querySelector('#snapshots')?.setAttribute('count', String(p.snapshotCount ?? 0));
}

// ---------------------------------------------------------------------------
// Inner tabs
// ---------------------------------------------------------------------------

function drawInner(screen, body, state, refresh) {
  const host = body.querySelector('#nas-pool-tab-body');
  state.live = null;
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

function paintTopology(screen, body, state, refresh) {
  const host = body.querySelector('#nas-pool-tab-body');
  const p = state.res.pool;
  const admin = screen.isAdmin;
  const scan = p.scan || {};
  const free = state.freeDisks;
  const vdevs = p.vdevs || [];
  // `zpool status` knows the leaf, not the device behind it: media kind and
  // temperature (n06:223) come from the node's disk inventory, matched by id
  // or by kernel name (a partition leaf carries neither on its own).
  const inventory = new Map();
  for (const d of state.disks) {
    if (d.diskId) inventory.set(d.diskId, d);
    if (d.name) inventory.set(d.name, d);
  }

  const vdevHtml = (v) => {
    const raidz = /^raidz/.test(v.kind);
    const removable = v.role === 'cache' || v.role === 'log' || v.role === 'spare';
    const actions = [];
    // RAIDZ expansion is always on the group so the admin learns it exists;
    // without a free disk it is disabled with the reason.
    if (admin && raidz) actions.push(`<tf-button size="sm" variant="secondary" icon="plus" data-act="expand" data-vdev="${escapeAttr(v.id)}" ${free.length ? '' : `disabled title="${escapeAttr(T('pools.no_free_disks'))}"`}>${escapeHtml(T('pool.vdev_expand'))}</tf-button>`);
    if (admin && removable) actions.push(`<tf-button size="sm" variant="ghost" tone="critical" icon="trash" data-act="remove-vdev" data-vdev="${escapeAttr(v.id)}">${escapeHtml(T('pool.vdev_remove'))}</tf-button>`);
    const disks = (v.disks || []).map((d) => {
      const bad = d.state !== 'online';
      const resilvering = scan.kind === 'resilver' && scan.status === 'running' && bad;
      const errs = (Number(d.readErrors) || 0) + (Number(d.writeErrors) || 0) + (Number(d.cksumErrors) || 0);
      const acts = [];
      if (admin) {
        if (free.length && v.role !== 'spare') acts.push(`<tf-button size="sm" variant="ghost" icon="refresh" data-act="replace" data-vdev="${escapeAttr(v.id)}" data-device="${escapeAttr(d.name)}" title="${escapeAttr(T('pool.disk_replace'))}"></tf-button>`);
        if (d.state === 'online') acts.push(`<tf-button size="sm" variant="ghost" icon="ban" data-act="offline" data-device="${escapeAttr(d.name)}" title="${escapeAttr(T('pool.disk_offline'))}"></tf-button>`);
        else if (d.state === 'offline') acts.push(`<tf-button size="sm" variant="ghost" icon="play" data-act="online" data-device="${escapeAttr(d.name)}" title="${escapeAttr(T('pool.disk_online'))}"></tf-button>`);
        if (errs) acts.push(`<tf-button size="sm" variant="ghost" icon="check" data-act="clear" data-device="${escapeAttr(d.name)}" title="${escapeAttr(T('pool.disk_clear'))}"></tf-button>`);
      }
      if (d.diskId) acts.push(`<tf-button size="sm" variant="ghost" icon="chevron-right" data-act="disk" data-disk="${escapeAttr(d.diskId)}" title="${escapeAttr(T('disks.details'))}"></tf-button>`);
      const inv = (d.diskId && inventory.get(d.diskId)) || inventory.get(d.name);
      const sub = [fmtBytes(d.sizeBytes), inv?.temperatureC == null ? null : `${inv.temperatureC}°C`, d.note || null].filter(Boolean).join(' · ');
      return `
        <div class="disk-cell ${bad ? 'faulted' : ''} ${resilvering ? 'resilver' : ''} ${v.role === 'spare' ? 'spare' : ''}">
          <div class="dc-main">
            <div class="dc-name"><span class="health-dot ${stateTone(d.state)}"></span><span class="mono">${escapeHtml(d.name)}</span>${bad ? `<tf-chip size="sm" status="${stateTone(d.state)}" label="${escapeAttr(stateLabel(d.state))}"></tf-chip>` : ''}</div>
            <div class="dc-sub">${escapeHtml(sub)}</div>
            <div class="dc-sub mono ${errs ? 'num-err' : ''}">R ${Number(d.readErrors) || 0} · W ${Number(d.writeErrors) || 0} · CKSUM ${Number(d.cksumErrors) || 0}</div>
          </div>
          ${inv ? `<span class="disk-kind ${escapeAttr(inv.kind)}">${escapeHtml(inv.kind)}</span>` : ''}
          <div class="dc-actions">${acts.join('')}</div>
        </div>`;
    }).join('');
    const hintKey = VDEV_HINTS[v.role];
    const hint = hintKey ? T(hintKey, { pool: p.name }) : T('pool.tolerance_hint', { n: v.faultTolerance });
    return `
      <div class="vdev-group" data-vdev="${escapeAttr(v.id)}">
        <div class="vg-head">
          <span class="vg-type">${escapeHtml(v.role === 'data' ? layoutLabel(v.kind) : `${T('pool.role_' + v.role)} · ${layoutLabel(v.kind)}`)}</span>
          <span class="mono text-3">${escapeHtml(v.id)}</span>
          ${v.state === 'online' ? '' : stateChipHtml(v.state)}
          <span class="hint">${escapeHtml(hint)}</span>
          <span class="spacer"></span>
          ${actions.join('')}
        </div>
        <div class="disk-cells">${disks || `<div class="muted">${escapeHtml(T('pool.vdev_empty'))}</div>`}</div>
      </div>`;
  };

  // The three shortcuts of the mockup; "Dodaj vdev" opens the dialog with
  // the role select, so log/special/dedup groups are reachable from it.
  const freeNvme = free.some((d) => d.kind === 'nvme');
  const addButton = (role, icon, enabled, reason) => `<tf-button size="sm" variant="secondary" icon="${icon}" data-act="add-vdev" data-role="${role}" ${enabled ? '' : `disabled title="${escapeAttr(reason)}"`}>${escapeHtml(T('pool.add_' + role))}</tf-button>`;
  const addButtons = admin
    ? addButton('data', 'plus', free.length > 0, T('pools.no_free_disks')) + addButton('cache', 'zap', freeNvme, T('pool.no_free_nvme')) + addButton('spare', 'cylinder', free.length > 0, T('pools.no_free_disks'))
    : '';

  const scanRunning = scan.status === 'running' || scan.status === 'paused';
  const scrubButtons = !admin ? '' : scan.status === 'running'
    ? `<tf-button variant="ghost" size="sm" icon="pause" data-act="scrub-pause">${escapeHtml(T('pool.scrub_pause'))}</tf-button>
       <tf-button variant="ghost" size="sm" icon="stop" data-act="scrub-stop">${escapeHtml(T('pool.scrub_stop'))}</tf-button>`
    : scan.status === 'paused'
      ? `<tf-button variant="secondary" size="sm" icon="play" data-act="scrub-resume">${escapeHtml(T('pool.scrub_resume'))}</tf-button>
         <tf-button variant="ghost" size="sm" icon="stop" data-act="scrub-stop">${escapeHtml(T('pool.scrub_stop'))}</tf-button>`
      : `<tf-button variant="secondary" size="sm" icon="play" data-act="scrub-start">${escapeHtml(T('pool.scrub_now'))}</tf-button>`;
  const lastScrub = p.lastScrubAt
    ? [fmtDate(p.lastScrubAt), scan.status === 'finished' && scan.durationSecs ? fmtDuration(scan.durationSecs) : '', scan.status === 'finished' ? `<span class="${scan.errors ? 'num-err' : 'num-ok'}">${escapeHtml(T('pools.scrub_errors', { n: Number(scan.errors) || 0 }))}</span>` : ''].filter(Boolean).join(' · ')
    : escapeHtml(T('pools.never'));
  const scrubRows = [
    ['check', T('pool.scrub_last'), lastScrub],
    ['clock', T('pool.scrub_schedule'), `<span class="sched-pill" ${admin ? 'data-act="scrub-schedule" role="button"' : ''}>${sprite('clock')} ${escapeHtml(p.scrubSchedule ? fmtSchedule(p.scrubSchedule) : T('schedule.none'))}</span>${p.nextScrubAt ? ` <span class="text-3">${escapeHtml(fmtIn(p.nextScrubAt))}</span>` : ''}`],
    ['cylinder', T('pool.errors'), `<span class="mono ${(p.readErrors || p.writeErrors || p.cksumErrors) ? 'num-err' : ''}">${Number(p.readErrors) || 0} / ${Number(p.writeErrors) || 0} / ${Number(p.cksumErrors) || 0}</span>`],
    ['zap', T('pool.autotrim'), p.autotrim ? `<span class="num-ok">${escapeHtml(T('schedule.on'))}</span>` : escapeHtml(T('schedule.off'))],
  ];
  const io = p.io || {};
  const ioRows = [
    [T('pool.io_throughput'), `${fmtMBps(io.readBps)} / ${fmtMBps(io.writeBps)} MB/s`],
    [T('pool.io_iops'), `${Math.round(Number(io.readIops) || 0)} / ${Math.round(Number(io.writeIops) || 0)}`],
    [T('pool.io_latency'), `${(Number(io.readLatencyMs) || 0).toFixed(1)} / ${(Number(io.writeLatencyMs) || 0).toFixed(1)} ms`],
  ];

  host.innerHTML = `
    <div class="stack">
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('layers')} ${escapeHtml(T('pool.topology_title'))}</div>
          ${addButtons ? `<div class="actions">${addButtons}</div>` : ''}
        </div>
        ${vdevs.map(vdevHtml).join('') || `<div class="muted">${escapeHtml(T('pool.no_vdevs'))}</div>`}
      </div>
      <div class="grid-2 pool-topology">
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('shield')} ${escapeHtml(T('pool.scrub_title_card'))}</div>
            <div class="actions">${scanRunning ? `<tf-chip status="${scan.status === 'paused' ? 'warn' : 'accent'}" label="${escapeAttr(T('pools.scan_' + scan.kind, { pct: Math.round(Number(scan.progressPct) || 0) }))}"></tf-chip>` : ''}${scrubButtons}</div></div>
          ${scanRunning ? `<tf-progress-bar value="${Math.round(Number(scan.progressPct) || 0)}" tone="accent" label="${escapeAttr(T('pool.scan_eta', { eta: fmtDuration(scan.etaSecs), scanned: fmtBytes(scan.scannedBytes) }))}"></tf-progress-bar>` : ''}
          <div class="stat-rows">${scrubRows.map(([icon, k, v]) => `<div class="sr"><span class="k">${sprite(icon)} ${escapeHtml(k)}</span><span class="v">${v}</span></div>`).join('')}</div>
        </div>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('trend')} ${escapeHtml(T('pool.io_title'))}</div><span class="hint">${escapeHtml(T('pool.io_hint'))}</span></div>
          <div class="stat-rows">${ioRows.map(([k, v]) => `<div class="sr"><span class="k">${escapeHtml(k)}</span><span class="v mono">${escapeHtml(v)}</span></div>`).join('')}</div>
          <tf-stream-chart id="nas-pool-io-live" class="mt-sm"></tf-stream-chart>
          <div class="live-label"><span class="live-dot"></span>${escapeHtml(T('overview.live_window', { w: fmtDuration(IO_WINDOW_SECS) }))}</div>
        </div>
      </div>
      ${propertiesSectionHtml(screen, state)}
    </div>`;
  // The topology pane is rebuilt on every poll, so its stream chart is seeded
  // from the samples kept on `state` instead of owning them.
  mountIoChart(host.querySelector('#nas-pool-io-live'), 72, state);
  paintProperties(screen, host, state, refresh);

  for (const act of ['start', 'pause', 'resume', 'stop']) {
    host.querySelector(`[data-act="scrub-${act}"]`)?.addEventListener('click', () => scrubAction(screen, p.name, act, refresh));
  }
  host.querySelector('[data-act="scrub-schedule"]')?.addEventListener('click', () => openScrubScheduleEditor(screen, p, refresh));
  host.querySelectorAll('[data-act="add-vdev"]').forEach((b) => b.addEventListener('click', () => openAddVdevDialog(screen, p, b.dataset.role, free, refresh)));
  host.querySelectorAll('[data-act="expand"]').forEach((b) => b.addEventListener('click', () => {
    const v = vdevs.find((x) => x.id === b.dataset.vdev);
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
  }));
  host.querySelectorAll('[data-act="remove-vdev"]').forEach((b) => b.addEventListener('click', async () => {
    const v = vdevs.find((x) => x.id === b.dataset.vdev);
    const ok = await TfWindow.confirm({ title: T('pool.vdev_remove'), message: T('pool.vdev_remove_confirm', { id: v.id, role: T('pool.role_' + v.role) }), confirmLabel: T('pool.vdev_remove'), cancelLabel: I18n.t('common.cancel'), danger: true });
    if (!ok) return;
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolRemoveVdevRequest', { name: p.name, vdevId: v.id, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('pool.vdev_remove'));
    followResponse(screen, res, refresh, T('pool.vdev_remove_done'));
  }));
  host.querySelectorAll('[data-act="replace"]').forEach((b) => b.addEventListener('click', () => {
    const v = vdevs.find((x) => x.id === b.dataset.vdev);
    const d = (v.disks || []).find((x) => x.name === b.dataset.device);
    openReplaceWizard(screen, { pool: p, vdev: v, disk: d, freeDisks: free, disks: state.disks, onDone: refresh });
  }));
  for (const action of ['offline', 'online', 'clear']) {
    host.querySelectorAll(`[data-act="${action}"]`).forEach((b) => b.addEventListener('click', async () => {
      const device = b.dataset.device;
      if (action === 'offline') {
        const ok = await TfWindow.confirm({ title: T('pool.disk_offline'), message: T('pool.disk_offline_confirm', { device, ft: p.faultTolerance }), confirmLabel: T('pool.disk_offline'), cancelLabel: I18n.t('common.cancel'), danger: true });
        if (!ok) return;
      }
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolDeviceStateRequest', { name: p.name, device, action, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('pool.disk_' + action));
      followResponse(screen, res, refresh, T('pool.disk_' + action + '_done', { device }));
    }));
  }
  host.querySelectorAll('[data-act="disk"]').forEach((b) => b.addEventListener('click', () => screen.openDisk(b.dataset.disk)));
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

// ---------------------------------------------------------------------------
// Stats (live throughput + 24h history)
// ---------------------------------------------------------------------------

function paintStats(body, state) {
  const host = body.querySelector('#nas-pool-tab-body');
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
// topology IO card and on the Statystyki tab, and the topology card is
// re-rendered on every poll, so the samples cannot live in the chart.
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
  val.innerHTML = `<span class="sw primary"></span>${escapeHtml(T('disk.legend_read'))} ${escapeHtml(fmtMBps(last.read))} MB/s&nbsp;&nbsp;<span class="sw info"></span>${escapeHtml(T('disk.legend_write'))} ${escapeHtml(fmtMBps(last.write))} MB/s`;
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
// panes emit the same markup from here and share `paintProperties`.
function propertiesSectionHtml(screen, state) {
  const p = state.res.pool;
  const childNames = (state.res.datasets || []).filter((d) => d.name !== p.name).map((d) => d.name.slice(p.name.length + 1)).join(', ') || '—';
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
      ${dangerRowHtml({ title: T('danger.destroy', { name: p.name }), desc: T('danger.destroy_desc', { names: childNames }), action: T('danger.destroy_action'), icon: 'trash', act: 'destroy' })}
    </div>` : ''}`;
}

function paintPropertiesTab(screen, body, state, refresh) {
  const host = body.querySelector('#nas-pool-tab-body');
  host.innerHTML = `<div class="stack">${propertiesSectionHtml(screen, state)}</div>`;
  paintProperties(screen, host, state, refresh);
}

// Fills the "Właściwości puli" table and wires the danger zone that follows
// it on the topology and properties tabs (n06).
function paintProperties(screen, host, state, refresh) {
  const p = state.res.pool;
  const admin = screen.isAdmin;
  const datasets = state.res.datasets || [];
  const table = host.querySelector('#nas-pool-props');
  const props = state.res.properties || [];
  const editable = (name) => admin && (name in POOL_PROPS || name in DATASET_PROPS);
  table.rowActions = (row) => {
    if (!editable(row._prop.name)) return null;
    const wrap = document.createElement('div');
    wrap.innerHTML = `<tf-button size="sm" variant="ghost" icon="edit" data-act="edit" title="${escapeAttr(I18n.t('common.edit'))}"></tf-button>`;
    wrap.querySelector('[data-act="edit"]').addEventListener('click', (e) => { e.stopPropagation(); openPropertyEditor(screen, p.name, row._prop, refresh); });
    return wrap;
  };
  table.rows = props.map((pr) => ({
    _prop: pr,
    name: `<span class="tf-table__cell--mono">${escapeHtml(pr.name)}</span>`,
    value: `<span class="tf-table__cell--mono">${escapeHtml(pr.value ?? '—')}</span>${pr.name === 'compression' && p.compressRatio ? ` <tf-chip size="sm" status="ok" label="${escapeAttr(T('pool.ratio_chip', { ratio: fmtRatio(p.compressRatio) }))}"></tf-chip>` : ''}${pr.inheritedFrom ? `<div class="tf-table__cell-sub">${escapeHtml(T('props.inherited_from', { from: pr.inheritedFrom }))}</div>` : ''}`,
  }));

  if (!admin) return;
  host.querySelector('[data-act="export"]').addEventListener('click', async () => {
    const force = Boolean(host.querySelector('#nas-export-force').checked);
    const ok = await TfWindow.confirm({ title: T('danger.export'), message: T('danger.export_confirm', { name: p.name }), confirmLabel: T('danger.export_action'), cancelLabel: I18n.t('common.cancel'), danger: true });
    if (!ok) return;
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolExportRequest', { name: p.name, force, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('danger.export'));
    followResponse(screen, res, () => { screen.pool = null; screen.clearTimers(); screen.setLocation(); screen.drawTab(); }, T('danger.export_done', { name: p.name }));
  });
  host.querySelector('[data-act="destroy"]').addEventListener('click', () => openPoolDestroyDialog(screen, p, datasets, () => {
    screen.pool = null;
    screen.clearTimers();
    screen.setLocation();
    screen.drawTab();
  }));
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
  const shown = datasets.slice(0, 8);
  const more = datasets.length - shown.length;
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
    if (state.diskIds.size && state.layout && !state.busy) btn.removeAttribute('disabled');
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
// Replace (n17c): pick the replacement (a hot-spare of the pool first, then
// the free disks) → the replace job → the resilver followed from `zpool
// status` until the vdev is whole again. The install-wizard shell keeps it
// consistent with pool creation.
export function openReplaceWizard(screen, { pool, vdev, disk, freeDisks, disks = [], onDone }) {
  if (screen.openWindow) { screen.openWindow.remove(); screen.openWindow = null; }
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
  win.setAttribute('title', T('replace.title', { device: disk.name, pool: pool.name, layout: layoutLabel(vdev.kind) }));
  win.setAttribute('icon', 'refresh');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '820');
  win.setAttribute('min-width', '640');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  screen.openWindow = win;

  // n17c shows the step rail alone in the window body — the window title
  // already names the disk, the pool and its layout.
  const header = () => `
    <div class="install-progress">${steps.map((s, i) => `<div class="install-step ${i === state.step ? 'active' : i < state.step ? 'done' : ''}"><span class="num">${i < state.step ? sprite('check') : i + 1}</span><span class="label">${escapeHtml(s)}</span></div>`).join('')}</div>`;

  const optionHtml = (d) => {
    const on = state.pick && state.pick.diskId === d.diskId;
    const name = d.spare
      ? `${escapeHtml(T('replace.spare_name', { name: d.name, pool: pool.name }))} <tf-chip size="sm" status="ok" dot label="${escapeAttr(T('replace.spare_ready'))}"></tf-chip>`
      : escapeHtml(T('replace.free_name', { name: d.name, size: fmtBytes(d.sizeBytes), model: d.model || '—' }));
    const sub = d.spare
      ? T('replace.spare_sub', { size: fmtBytes(d.sizeBytes), model: d.model || '—', serial: d.serial || '—' })
      : T('replace.free_sub', { serial: d.serial || '—' });
    return `
      <div class="target-option ${on ? 'checked' : ''} ${d.small ? 'disabled' : ''}" data-disk="${escapeAttr(d.diskId)}" ${d.small ? `title="${escapeAttr(T('pool.disk_too_small', { min: fmtBytes(minBytes) }))}"` : ''}>
        <tf-checkbox ${on ? 'checked' : ''} ${d.small ? 'disabled' : ''}></tf-checkbox>
        <div class="t-body">
          <div class="t-name">${name}</div>
          <div class="t-sub">${escapeHtml(sub)}${d.small ? ` · ${escapeHtml(T('pool.disk_too_small', { min: fmtBytes(minBytes) }))}` : ''}</div>
        </div>
      </div>`;
  };

  const explainHtml = () => (state.pick
    ? T('replace.explain', { old: escapeHtml(disk.name), new: escapeHtml(state.pick.name), pool: escapeHtml(pool.name), layout: escapeHtml(layoutLabel(vdev.kind)), ft: Math.max(0, (Number(vdev.faultTolerance) || 0) - 1) })
    : escapeHtml(T('replace.explain_pick')));

  const stepPick = () => `
    <div class="stack" id="nas-rp-list">${candidates.map(optionHtml).join('') || `<div class="muted">${escapeHtml(T('pools.no_free_disks'))}</div>`}</div>
    <div class="explain-box mt-md" id="nas-rp-explain">${explainHtml()}</div>
    <div class="wizard-warning mt-md">${sprite('alert')}<div>${escapeHtml(T('replace.warning', { device: disk.name }))}</div></div>`;

  const jobLog = () => `<pre class="job-log mono mt-sm">${escapeHtml((state.job?.log || []).join('\n'))}</pre>`;

  const stepRun = () => `
    <h2 class="wizard-section-title">${escapeHtml(T('replace.run_title', { old: disk.name, new: state.pick.name }))}</h2>
    <p class="wizard-section-sub">${escapeHtml(T('replace.sub_run'))}</p>
    ${state.job ? `<tf-progress-bar value="${Number(state.job.progressPct) || 0}" tone="accent" label="${escapeAttr(T('jobs.status_' + state.job.status))}"></tf-progress-bar>${jobLog()}` : `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`}`;

  const stepResilver = () => {
    if (state.result) {
      const ok = state.result.ok;
      return `<div class="result-box ${ok ? 'ok' : 'err'}">${sprite(ok ? 'check-circle' : 'alert')}<h3>${escapeHtml(ok ? T('replace.done_title') : T('replace.failed_title'))}</h3><p>${escapeHtml(state.result.detail || '')}</p></div>${state.job ? jobLog() : ''}`;
    }
    const scan = state.scan || {};
    const pctDone = Math.round(Number(scan.progressPct) || 0);
    return `
      <h2 class="wizard-section-title">${escapeHtml(T('replace.step_resilver'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(T('replace.sub_resilver'))}</p>
      <tf-progress-bar value="${pctDone}" tone="accent" label="${escapeAttr(T('replace.resilver_progress', { pct: pctDone, eta: fmtDuration(scan.etaSecs) }))}"></tf-progress-bar>
      ${warningHtml('info', T('replace.warning', { device: disk.name }))}`;
  };

  const footer = () => {
    const running = state.step > 0 && !state.result;
    const finished = Boolean(state.result);
    let next;
    if (finished) next = `<tf-button variant="primary" icon="check" data-wizard-next>${escapeHtml(I18n.t('common.close'))}</tf-button>`;
    else if (state.step === 0) next = `<tf-button variant="primary" icon="play" data-wizard-next ${state.pick ? '' : 'disabled'}>${escapeHtml(T('replace.start'))}</tf-button>`;
    else next = '';
    return `
      <tf-button variant="ghost" data-wizard-cancel ${running ? 'disabled' : ''}>${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="ghost" icon="chevron-left" data-wizard-back disabled>${escapeHtml(I18n.t('common.back'))}</tf-button>
      <span class="spacer"></span>
      ${next}`;
  };

  const draw = () => {
    win.innerHTML = `<div slot="body">${header()}<div class="install-step-body">${[stepPick, stepRun, stepResilver][state.step]()}</div></div><div slot="footer">${footer()}</div>`;
    const list = win.querySelector('#nas-rp-list');
    if (list) {
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
    win.querySelector('[data-wizard-cancel]').addEventListener('click', () => win.close());
    win.querySelector('[data-wizard-next]')?.addEventListener('click', next);
  };

  // One replacement at a time: picking an option unchecks the others.
  const pick = (diskId) => {
    state.pick = candidates.find((d) => d.diskId === diskId && !d.small) || null;
    win.querySelectorAll('.target-option[data-disk]').forEach((o) => {
      const on = state.pick && o.dataset.disk === state.pick.diskId;
      o.classList.toggle('checked', Boolean(on));
      o.querySelector('tf-checkbox').checked = Boolean(on);
    });
    win.querySelector('#nas-rp-explain').innerHTML = explainHtml();
    const btn = win.querySelector('[data-wizard-next]');
    if (state.pick) btn.removeAttribute('disabled'); else btn.setAttribute('disabled', '');
  };

  const next = async () => {
    if (state.result) { win.close(); return; }
    if (state.step !== 0 || !state.pick) return;
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolReplaceDiskRequest', { name: pool.name, old: disk.name, diskId: state.pick.diskId, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('replace.title', { device: disk.name, pool: pool.name, layout: layoutLabel(vdev.kind) }));
    if (!res) return;
    state.step = 1;
    state.job = res.job || null;
    draw();
    if (state.job) await pollJob(); else await pollResilver();
  };

  const pollJob = async () => {
    if (!win.isConnected || !state.job) return;
    try {
      const r = await screen.nas('tentaNasJobGetRequest', { jobId: state.job.jobId });
      state.job = r.job;
    } catch (e) {
      state.step = 2;
      state.result = { ok: false, detail: errMessage(e) };
      draw();
      return;
    }
    const s = state.job.status;
    if (s === 'running' || s === 'queued') {
      draw();
      state.timer = setTimeout(pollJob, POLL_JOB_MODAL_MS);
      return;
    }
    if (s !== 'succeeded' && s !== 'done') {
      state.step = 2;
      state.result = { ok: false, detail: state.job.error || T('jobs.status_' + s) };
      draw();
      return;
    }
    state.step = 2;
    draw();
    await pollResilver();
  };

  // The replace job returns once `zpool replace` is accepted; the resilver
  // it starts is the pool's scan, so the last step reads `PoolGet` until
  // that scan is no longer a running resilver.
  const pollResilver = async () => {
    if (!win.isConnected) return;
    try {
      const r = await screen.nas('tentaNasPoolGetRequest', { name: pool.name });
      state.scan = r.pool?.scan || {};
    } catch (e) {
      state.result = { ok: false, detail: errMessage(e) };
      draw();
      return;
    }
    const active = state.scan.kind === 'resilver' && (state.scan.status === 'running' || state.scan.status === 'paused');
    if (active) {
      draw();
      state.timer = setTimeout(pollResilver, POLL_JOB_MODAL_MS);
      return;
    }
    const failed = state.scan.kind === 'resilver' && Number(state.scan.errors) > 0;
    state.result = failed
      ? { ok: false, detail: T('pools.scrub_errors', { n: Number(state.scan.errors) || 0 }) }
      : { ok: true, detail: T('replace.done_detail', { device: disk.name }) };
    draw();
    if (onDone) onDone(state.job);
  };

  win.addEventListener('close-request', () => {
    if (state.timer) clearTimeout(state.timer);
    if (screen.openWindow === win) screen.openWindow = null;
  });
  draw();
  document.body.appendChild(win);
  return win;
}
