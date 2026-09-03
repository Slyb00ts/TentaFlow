// ===== File: modules/tentanas/pool-detail.js — one pool (n06): header, inner tabs (topology, datasets, snapshots, stats, properties), scrub card, vdev actions, danger zone =====
//
// The detail screen keeps one `PoolGet` result as its state and repaints
// the header, KPIs and the active inner tab from it on every poll; the
// datasets and snapshots tabs own their own lists and only borrow the pool
// name and the dataset list from here.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import {
  T, sprite, POLL_POOLS_MS, POLL_JOB_MODAL_MS, IO_WINDOW_SECS, ADMIN_TIMEOUT_MS, parseServerTs,
  fmtDate, fmtAgo, fmtIn, fmtDuration, fmtBytes, fmtMBps, fmtRatio, pct, healthClass, healthChip, errMessage,
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
  const state = { name, res: null, freeDisks: [], live: null };
  try {
    [state.res, state.freeDisks] = await Promise.all([
      screen.nas('tentaNasPoolGetRequest', { name }),
      loadFreeDisks(screen),
    ]);
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
      <div class="section-card" id="nas-pool-head"></div>
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
      [state.res, state.freeDisks] = await Promise.all([
        screen.nas('tentaNasPoolGetRequest', { name }),
        loadFreeDisks(screen),
      ]);
    } catch (e) {
      toast(errMessage(e), 'error');
      return;
    }
    if (screen.disposed || !body.isConnected) return;
    paintHeader(screen, body, state, refresh);
    if (screen.poolTab === 'topology') paintTopology(screen, body, state, refresh);
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

  paintHeader(screen, body, state, refresh);
  drawInner(screen, body, state, refresh);
  screen.later(poll, POLL_POOLS_MS);
}

async function loadFreeDisks(screen) {
  const r = await screen.nas('tentaNasDisksListRequest', {});
  return (r.disks || []).filter((d) => d.role === 'free');
}

const crumbs = (name) => `<div class="crumbs"><a data-act="back">${escapeHtml(T('tabs.pools'))}</a><span class="sep">›</span><span class="mono">${escapeHtml(name)}</span></div>`;

function wireBack(screen, body) {
  body.querySelector('[data-act="back"]').addEventListener('click', () => {
    screen.pool = null;
    screen.dataset = null;
    screen.clearTimers();
    screen.setLocation();
    screen.drawTab();
  });
}

// ---------------------------------------------------------------------------
// Header + KPIs
// ---------------------------------------------------------------------------

function paintHeader(screen, body, state, refresh) {
  const p = state.res.pool;
  const scan = p.scan || {};
  const health = healthChip(p.health);
  const admin = screen.isAdmin;
  const scrubButtons = scan.status === 'running'
    ? `<tf-button variant="ghost" size="sm" icon="pause" data-act="scrub-pause">${escapeHtml(T('pool.scrub_pause'))}</tf-button>
       <tf-button variant="ghost" size="sm" icon="stop" data-act="scrub-stop">${escapeHtml(T('pool.scrub_stop'))}</tf-button>`
    : scan.status === 'paused'
      ? `<tf-button variant="ghost" size="sm" icon="play" data-act="scrub-resume">${escapeHtml(T('pool.scrub_resume'))}</tf-button>
         <tf-button variant="ghost" size="sm" icon="stop" data-act="scrub-stop">${escapeHtml(T('pool.scrub_stop'))}</tf-button>`
      : `<tf-button variant="ghost" size="sm" icon="shield" data-act="scrub-start">${escapeHtml(T('pool.scrub_now'))}</tf-button>`;
  body.querySelector('#nas-pool-head').innerHTML = `
    <div class="section-card-head">
      <div class="title"><span class="health-dot ${healthClass(p.health)}"></span> <span class="mono">${escapeHtml(p.name)}</span>
        <tf-chip status="${health.status}" dot label="${escapeAttr(health.label)}"></tf-chip>
        ${stateChipHtml(p.state)}
        <tf-chip status="info" label="${escapeAttr(layoutLabel(p.layout))} · ${p.dataDisks}× · ${escapeAttr(T('pool.tolerance_short', { n: p.faultTolerance }))}"></tf-chip>
        ${p.encryption && p.encryption !== 'off' ? `<tf-chip status="info" icon="lock" label="${escapeAttr(p.encryption)}"></tf-chip>` : ''}
        ${p.readOnly ? `<tf-chip status="warn" label="${escapeAttr(T('pool.read_only'))}"></tf-chip>` : ''}
      </div>
      <div class="actions">${admin ? scrubButtons : ''}<tf-button variant="ghost" size="sm" icon="refresh" data-act="refresh"></tf-button></div>
    </div>
    ${p.healthReason ? `<div class="pc-reason ${healthClass(p.health)}">${sprite('alert')} ${escapeHtml(p.healthReason)}</div>` : ''}
    <div class="text-3">${escapeHtml(T('pool.guid'))} <span class="mono">${escapeHtml(p.guid || '—')}</span> · ashift ${escapeHtml(String(p.ashift ?? '—'))} · ${escapeHtml(T('pool.datasets_snapshots', { d: p.datasetCount, s: p.snapshotCount }))}</div>`;
  const head = body.querySelector('#nas-pool-head');
  head.querySelector('[data-act="refresh"]').addEventListener('click', refresh);
  for (const act of ['start', 'pause', 'resume', 'stop']) {
    head.querySelector(`[data-act="scrub-${act}"]`)?.addEventListener('click', () => scrubAction(screen, p.name, act, refresh));
  }

  const usedPct = pct(p.usedBytes, p.usableBytes);
  const io = p.io || {};
  body.querySelector('#nas-pool-kpi').innerHTML = `
    <tf-stat-card label="${escapeAttr(T('pool.kpi_capacity'))}" value="${usedPct}" suffix="%" icon="database" ${usedPct > 90 ? 'accent="danger"' : usedPct > 75 ? 'accent="warning"' : ''} delta="${escapeAttr(fmtBytes(p.usedBytes) + ' / ' + fmtBytes(p.usableBytes))}"></tf-stat-card>
    <tf-stat-card label="${escapeAttr(T('pool.kpi_free'))}" value="${escapeAttr(fmtBytes(p.availableBytes))}" icon="cylinder"></tf-stat-card>
    <tf-stat-card label="${escapeAttr(T('pool.kpi_iops'))}" value="${Math.round((Number(io.readIops) || 0) + (Number(io.writeIops) || 0))}" icon="zap" delta="${escapeAttr(T('pool.kpi_iops_delta', { r: fmtMBps(io.readBps), w: fmtMBps(io.writeBps) }))}"></tf-stat-card>
    <tf-stat-card label="${escapeAttr(T('pool.kpi_fragmentation'))}" value="${Math.round(Number(p.fragmentationPct) || 0)}" suffix="%" icon="grid-2x2" ${Number(p.fragmentationPct) > 50 ? 'accent="warning"' : ''}></tf-stat-card>
    <tf-stat-card label="${escapeAttr(T('pool.kpi_compress'))}" value="${escapeAttr(fmtRatio(p.compressRatio))}" icon="min" delta="${escapeAttr(p.compression || 'off')}"></tf-stat-card>`;

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
      paintProperties(screen, body, state, refresh);
      return;
    default:
      paintTopology(screen, body, state, refresh);
  }
}

// ---------------------------------------------------------------------------
// Topology (vdevs, disks, scrub, IO, alerts)
// ---------------------------------------------------------------------------

function paintTopology(screen, body, state, refresh) {
  const host = body.querySelector('#nas-pool-tab-body');
  const p = state.res.pool;
  const admin = screen.isAdmin;
  const scan = p.scan || {};
  const free = state.freeDisks;
  const vdevs = p.vdevs || [];

  const vdevHtml = (v) => {
    const raidz = /^raidz/.test(v.kind);
    const removable = v.role === 'cache' || v.role === 'log' || v.role === 'spare';
    const actions = [];
    if (admin && raidz && free.length) actions.push(`<tf-button size="sm" variant="ghost" icon="plus" data-act="expand" data-vdev="${escapeAttr(v.id)}">${escapeHtml(T('pool.vdev_expand'))}</tf-button>`);
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
      return `
        <div class="disk-cell ${bad ? 'faulted' : ''} ${resilvering ? 'resilver' : ''} ${v.role === 'spare' ? 'spare' : ''}">
          <div class="dc-main">
            <div class="dc-name"><span class="health-dot ${stateTone(d.state)}"></span><span class="mono">${escapeHtml(d.name)}</span><tf-chip size="sm" status="${stateTone(d.state)}" label="${escapeAttr(stateLabel(d.state))}"></tf-chip></div>
            <div class="dc-sub">${escapeHtml([fmtBytes(d.sizeBytes), d.path].filter(Boolean).join(' · '))}${d.note ? ` · ${escapeHtml(d.note)}` : ''}</div>
            <div class="dc-sub mono ${errs ? 'num-err' : ''}">R ${Number(d.readErrors) || 0} · W ${Number(d.writeErrors) || 0} · CKSUM ${Number(d.cksumErrors) || 0}</div>
          </div>
          <div class="dc-actions">${acts.join('')}</div>
        </div>`;
    }).join('');
    return `
      <div class="vdev-group" data-vdev="${escapeAttr(v.id)}">
        <div class="vg-head">
          <span class="vg-type">${escapeHtml(T('pool.role_' + v.role))} · ${escapeHtml(layoutLabel(v.kind))}</span>
          <span class="mono text-3">${escapeHtml(v.id)}</span>
          ${stateChipHtml(v.state)}
          <span class="hint">${escapeHtml(T('pool.tolerance_value', { n: v.faultTolerance }))}</span>
          <span class="spacer"></span>
          ${actions.join('')}
        </div>
        <div class="disk-cells">${disks || `<div class="muted">${escapeHtml(T('pool.vdev_empty'))}</div>`}</div>
      </div>`;
  };

  const addButtons = admin && free.length
    ? ['data', 'cache', 'log', 'spare', 'special'].map((role) => `<tf-button size="sm" variant="secondary" icon="plus" data-act="add-vdev" data-role="${role}">${escapeHtml(T('pool.add_' + role))}</tf-button>`).join('')
    : '';

  const scanRunning = scan.status === 'running' || scan.status === 'paused';
  const scrubRows = [
    [T('pool.scrub_last'), p.lastScrubAt ? `${fmtAgo(p.lastScrubAt)} <span class="text-3">${fmtDate(p.lastScrubAt)}</span>` : T('pools.never')],
    [T('pool.scrub_result'), scan.status === 'finished' ? (scan.errors ? `<span class="num-err">${escapeHtml(T('pools.scrub_errors', { n: scan.errors }))}</span>` : `<span class="num-ok">${escapeHtml(T('pool.scrub_no_errors'))}</span>`) + (scan.durationSecs ? ` · ${fmtDuration(scan.durationSecs)}` : '') : '—'],
    [T('pool.scrub_schedule'), `<span class="sched-pill">${sprite('clock')} ${escapeHtml(p.scrubSchedule ? fmtSchedule(p.scrubSchedule) : T('schedule.none'))}</span>${p.nextScrubAt ? ` <span class="text-3">${escapeHtml(fmtIn(p.nextScrubAt))}</span>` : ''}${admin ? ` <tf-button size="sm" variant="ghost" icon="edit" data-act="scrub-schedule"></tf-button>` : ''}`],
    [T('pool.errors'), `<span class="mono ${(p.readErrors || p.writeErrors || p.cksumErrors) ? 'num-err' : ''}">R ${Number(p.readErrors) || 0} · W ${Number(p.writeErrors) || 0} · CKSUM ${Number(p.cksumErrors) || 0}</span>`],
    [T('pool.autotrim'), p.autotrim ? I18n.t('common.yes') : I18n.t('common.no')],
  ];
  const io = p.io || {};
  const ioRows = [
    [T('pool.io_read'), `${fmtMBps(io.readBps)} MB/s · ${Math.round(Number(io.readIops) || 0)} IOPS`],
    [T('pool.io_write'), `${fmtMBps(io.writeBps)} MB/s · ${Math.round(Number(io.writeIops) || 0)} IOPS`],
    [T('pool.io_latency'), `${(Number(io.readLatencyMs) || 0).toFixed(1)} / ${(Number(io.writeLatencyMs) || 0).toFixed(1)} ms`],
    [T('pool.fragmentation'), `${Math.round(Number(p.fragmentationPct) || 0)}%`],
    [T('pool.dedup'), fmtRatio(p.dedupRatio)],
  ];

  host.innerHTML = `
    <div class="grid-2 pool-topology">
      <div class="stack">
        ${vdevs.map(vdevHtml).join('') || `<div class="muted">${escapeHtml(T('pool.no_vdevs'))}</div>`}
        ${addButtons ? `<div class="row wrap">${addButtons}</div>` : ''}
      </div>
      <div class="stack">
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('shield')} ${escapeHtml(T('pool.scrub_title_card'))}</div>
            ${scanRunning ? `<tf-chip status="${scan.status === 'paused' ? 'warn' : 'accent'}" label="${escapeAttr(T('pools.scan_' + scan.kind, { pct: Math.round(Number(scan.progressPct) || 0) }))}"></tf-chip>` : ''}</div>
          ${scanRunning ? `<tf-progress-bar value="${Math.round(Number(scan.progressPct) || 0)}" tone="accent" label="${escapeAttr(T('pool.scan_eta', { eta: fmtDuration(scan.etaSecs), scanned: fmtBytes(scan.scannedBytes) }))}"></tf-progress-bar>` : ''}
          <div class="stat-rows">${scrubRows.map(([k, v]) => `<div class="sr"><span class="k">${escapeHtml(k)}</span><span class="v">${v}</span></div>`).join('')}</div>
        </div>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('trend')} ${escapeHtml(T('pool.io_title'))}</div></div>
          <div class="stat-rows">${ioRows.map(([k, v]) => `<div class="sr"><span class="k">${escapeHtml(k)}</span><span class="v mono">${escapeHtml(v)}</span></div>`).join('')}</div>
        </div>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('bell')} ${escapeHtml(T('alerts.title'))}</div></div>
          <div id="nas-pool-alerts"></div>
        </div>
      </div>
    </div>`;
  screen.renderAlertList(host.querySelector('#nas-pool-alerts'), state.res.alerts || [], refresh);

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
    openReplaceWizard(screen, { pool: p, vdev: v, disk: d, freeDisks: free, onDone: refresh });
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
  const live = host.querySelector('#nas-pool-live');
  live.height = 170;
  live.window = IO_WINDOW_SECS;
  live.legend = { position: 'none' };
  live.tooltip = { valueFormat: (v) => `${fmtMBps(v)} MB/s` };
  live.yAxis = { min: 0, ticks: 4, format: (v) => fmtMBps(v) };
  live.series = [
    { id: 'read', name: T('disk.legend_read'), tone: 'primary', style: 'solid', showInLegend: false, points: [] },
    { id: 'write', name: T('disk.legend_write'), tone: 'info', style: 'solid', showInLegend: false, points: [] },
  ];
  state.live = live;
  pushLiveSample(body, state);

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

function pushLiveSample(body, state) {
  const live = state.live;
  if (!live || !live.isConnected) return;
  const io = state.res.pool.io || {};
  const read = Number(io.readBps) || 0;
  const write = Number(io.writeBps) || 0;
  live.push(Date.now(), { read, write });
  const val = body.querySelector('#nas-pool-live-val');
  if (val) val.innerHTML = `<span class="sw primary"></span>${escapeHtml(T('disk.legend_read'))} ${escapeHtml(fmtMBps(read))} MB/s&nbsp;&nbsp;<span class="sw info"></span>${escapeHtml(T('disk.legend_write'))} ${escapeHtml(fmtMBps(write))} MB/s`;
}

// ---------------------------------------------------------------------------
// Properties + danger zone
// ---------------------------------------------------------------------------

export const sourceChipHtml = (source) => `<tf-chip size="sm" status="${source === 'local' ? 'accent' : 'info'}" label="${escapeAttr(T('props.source_' + (source || 'default')))}"></tf-chip>`;

function paintProperties(screen, body, state, refresh) {
  const host = body.querySelector('#nas-pool-tab-body');
  const p = state.res.pool;
  const admin = screen.isAdmin;
  const datasets = state.res.datasets || [];
  host.innerHTML = `
    <div class="stack">
      <div class="section-card">
        <div class="section-card-head"><div class="title">${sprite('settings')} ${escapeHtml(T('props.title'))}</div><span class="hint">${escapeHtml(T('props.hint'))}</span></div>
        <tf-table id="nas-pool-props" empty-message="${escapeAttr(T('props.none'))}">
          <tf-column key="name" label="${escapeAttr(T('props.col_name'))}" renderer="html" width="260"></tf-column>
          <tf-column key="value" label="${escapeAttr(T('props.col_value'))}" renderer="html" fill></tf-column>
          <tf-column key="source" label="${escapeAttr(T('props.col_source'))}" renderer="html" width="140"></tf-column>
        </tf-table>
      </div>
      ${admin ? `
      <div class="section-card danger-zone">
        <h4>${sprite('alert')} ${escapeHtml(T('danger.title'))}</h4>
        ${dangerRowHtml({ title: T('danger.export'), desc: T('danger.export_desc'), action: T('danger.export_action'), icon: 'arrow-out', act: 'export' })}
        <tf-checkbox id="nas-export-force" label="${escapeAttr(T('danger.export_force'))}"></tf-checkbox>
        ${dangerRowHtml({ title: T('danger.destroy'), desc: T('danger.destroy_desc', { d: p.datasetCount, s: p.snapshotCount }), action: T('danger.destroy_action'), icon: 'trash', act: 'destroy' })}
      </div>` : ''}
    </div>`;

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
    value: `<span class="tf-table__cell--mono">${escapeHtml(pr.value ?? '—')}</span>${pr.inheritedFrom ? `<div class="tf-table__cell-sub">${escapeHtml(T('props.inherited_from', { from: pr.inheritedFrom }))}</div>` : ''}`,
    source: sourceChipHtml(pr.source),
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
  const disks = (pool.vdevs || []).flatMap((v) => v.disks || []);
  const bodyHtml = `
    ${warningHtml('danger', T('destroy_pool.warning', { name: pool.name }))}
    <div class="explain-box">${escapeHtml(T('destroy_pool.explain', { d: pool.datasetCount, s: pool.snapshotCount, size: fmtBytes(pool.usedBytes) }))}</div>
    ${shown.length ? `<ul class="loss-list">${shown.map((d) => `<li class="ll bad">${sprite('x')}<span><span class="mono">${escapeHtml(d.name)}</span> · ${escapeHtml(fmtBytes(d.usedBytes))}</span></li>`).join('')}${more > 0 ? `<li class="ll bad">${sprite('x')}<span>${escapeHtml(T('destroy_pool.more', { n: more }))}</span></li>` : ''}</ul>` : ''}
    <div class="kv-inline"><span class="k">${escapeHtml(T('destroy_pool.freed_disks'))}</span><span class="v mono">${escapeHtml(disks.map((d) => d.name).join(', ') || '—')}</span></div>`;
  return openRetypeDialog({
    title: T('destroy_pool.title', { name: pool.name }),
    icon: 'trash',
    name: pool.name,
    bodyHtml,
    confirmLabel: T('destroy_pool.confirm'),
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
      <div class="wizard-section-title">${escapeHtml(T('wizard_pool.disks_title'))}</div>
      <div class="disk-cells" id="nas-av-disks">${freeDisks.map((d) => `
        <div class="disk-cell" data-disk="${escapeAttr(d.diskId)}">
          <tf-checkbox></tf-checkbox>
          <div class="dc-main">
            <div class="dc-name"><span class="health-dot ${healthClass(d.health)}"></span><span class="mono">${escapeHtml(d.name)}</span><span class="disk-kind ${escapeAttr(d.kind)}">${escapeHtml(d.kind)}</span></div>
            <div class="dc-sub">${escapeHtml([fmtBytes(d.sizeBytes), d.model || ''].filter(Boolean).join(' · '))}</div>
          </div>
        </div>`).join('')}</div>
      <tf-select id="nas-av-layout" label="${escapeAttr(T('add_vdev.layout'))}"></tf-select>
      ${warningHtml('danger', T('wizard_pool.erase_warning'))}
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

// Replace (n17c): pick the replacement → confirm → resilver job followed in
// place. The install-wizard shell keeps it consistent with pool creation.
export function openReplaceWizard(screen, { pool, vdev, disk, freeDisks, onDone }) {
  if (screen.openWindow) { screen.openWindow.remove(); screen.openWindow = null; }
  const minBytes = Number(disk.sizeBytes) || 0;
  const state = { step: 0, pick: null, job: null, result: null, timer: null };
  const steps = [T('replace.step_pick'), T('replace.step_confirm'), T('replace.step_run')];
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('replace.title', { device: disk.name }));
  win.setAttribute('icon', 'refresh');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '820');
  win.setAttribute('min-width', '640');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  screen.openWindow = win;

  const header = () => `
    <div class="install-header">
      <div class="big-ico">${sprite('refresh')}</div>
      <div class="install-header-meta">
        <h1>${escapeHtml(T('replace.heading'))} <span class="version">${escapeHtml(disk.name)}</span></h1>
        <div class="sub">${escapeHtml(T('replace.sub', { pool: pool.name, vdev: vdev.id, state: stateLabel(disk.state) }))}</div>
      </div>
    </div>
    <div class="install-progress">${steps.map((s, i) => `<div class="install-step ${i === state.step ? 'active' : i < state.step ? 'done' : ''}"><span class="num">${i < state.step ? sprite('check') : i + 1}</span><span class="label">${escapeHtml(s)}</span></div>`).join('')}</div>`;

  const stepPick = () => `
    <div class="wizard-section-title">${escapeHtml(T('replace.pick_title'))}</div>
    <div class="wizard-section-sub">${escapeHtml(T('replace.pick_sub', { min: fmtBytes(minBytes) }))}</div>
    <div id="nas-rp-list">${freeDisks.map((d) => {
      const small = (Number(d.sizeBytes) || 0) < minBytes;
      return `<tf-option-row value="${escapeAttr(d.diskId)}" label="${escapeAttr(d.name)}" sub="${escapeAttr([fmtBytes(d.sizeBytes), d.model || '', d.serial || '', small ? T('pool.disk_too_small', { min: fmtBytes(minBytes) }) : ''].filter(Boolean).join(' · '))}" ${state.pick && state.pick.diskId === d.diskId ? 'selected' : ''} ${small ? 'disabled' : ''}></tf-option-row>`;
    }).join('') || `<div class="muted">${escapeHtml(T('pools.no_free_disks'))}</div>`}</div>`;

  const stepConfirm = () => `
    <div class="wizard-section-title">${escapeHtml(T('replace.confirm_title'))}</div>
    <div class="wizard-section-sub">${escapeHtml(T('replace.confirm_sub'))}</div>
    <div class="stat-rows">
      <div class="sr"><span class="k">${escapeHtml(T('replace.old'))}</span><span class="v mono">${escapeHtml(disk.name)} · ${escapeHtml(fmtBytes(disk.sizeBytes))} · ${escapeHtml(stateLabel(disk.state))}</span></div>
      <div class="sr"><span class="k">${escapeHtml(T('replace.new'))}</span><span class="v mono">${escapeHtml(state.pick.name)} · ${escapeHtml(fmtBytes(state.pick.sizeBytes))} · ${escapeHtml(state.pick.model || '')}</span></div>
      <div class="sr"><span class="k">${escapeHtml(T('replace.vdev'))}</span><span class="v">${escapeHtml(T('pool.role_' + vdev.role))} · ${escapeHtml(layoutLabel(vdev.kind))} · ${escapeHtml(vdev.id)}</span></div>
    </div>
    ${warningHtml('danger', T('replace.warning', { device: state.pick.name }))}
    <div class="explain-box mt-md">${escapeHtml(T('replace.explain', { ft: vdev.faultTolerance }))}</div>`;

  const stepRun = () => {
    if (state.result) {
      const ok = state.result.ok;
      return `<div class="result-box ${ok ? 'ok' : 'err'}">${sprite(ok ? 'check-circle' : 'alert')}<h3>${escapeHtml(ok ? T('replace.done_title') : T('replace.failed_title'))}</h3><p>${escapeHtml(state.result.detail || '')}</p></div>
        <pre class="job-log mono">${escapeHtml((state.job?.log || []).join('\n'))}</pre>`;
    }
    return `
      <div class="wizard-section-title">${escapeHtml(T('replace.run_title'))}</div>
      <div class="wizard-section-sub">${escapeHtml(T('replace.run_sub'))}</div>
      ${state.job ? `<tf-progress-bar value="${Number(state.job.progressPct) || 0}" tone="accent" label="${escapeAttr(T('jobs.status_' + state.job.status))}"></tf-progress-bar>
      <pre class="job-log mono mt-sm">${escapeHtml((state.job.log || []).join('\n'))}</pre>` : `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`}`;
  };

  const footer = () => {
    const running = state.step === 2 && !state.result;
    const finished = state.step === 2 && state.result;
    let next;
    if (finished) next = `<tf-button variant="primary" icon="check" data-wizard-next>${escapeHtml(I18n.t('common.close'))}</tf-button>`;
    else if (state.step === 1) next = `<tf-button variant="danger" icon="refresh" data-wizard-next>${escapeHtml(T('replace.confirm'))}</tf-button>`;
    else next = `<tf-button variant="primary" icon="chevron-right" data-wizard-next ${state.pick ? '' : 'disabled'}>${escapeHtml(I18n.t('common.next'))}</tf-button>`;
    return `
      <tf-button variant="ghost" data-wizard-cancel ${running ? 'disabled' : ''}>${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="ghost" icon="chevron-left" data-wizard-back ${state.step === 0 || state.step === 2 ? 'disabled' : ''}>${escapeHtml(I18n.t('common.back'))}</tf-button>
      <span class="spacer"></span>
      ${next}`;
  };

  const draw = () => {
    win.innerHTML = `<div slot="body">${header()}<div class="install-step-body">${[stepPick, stepConfirm, stepRun][state.step]()}</div></div><div slot="footer">${footer()}</div>`;
    win.querySelector('#nas-rp-list')?.addEventListener('option-select', (e) => {
      state.pick = freeDisks.find((d) => d.diskId === e.detail.value) || null;
      win.querySelectorAll('tf-option-row').forEach((r) => { r.selected = r.getAttribute('value') === e.detail.value; });
      const btn = win.querySelector('[data-wizard-next]');
      if (state.pick) btn.removeAttribute('disabled'); else btn.setAttribute('disabled', '');
    });
    win.querySelector('[data-wizard-cancel]').addEventListener('click', () => win.close());
    win.querySelector('[data-wizard-back]').addEventListener('click', () => { if (state.step === 1) { state.step = 0; draw(); } });
    win.querySelector('[data-wizard-next]').addEventListener('click', next);
  };

  const next = async () => {
    if (state.step === 0) { if (!state.pick) return; state.step = 1; draw(); return; }
    if (state.step === 2) { if (state.result) win.close(); return; }
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolReplaceDiskRequest', { name: pool.name, old: disk.name, diskId: state.pick.diskId, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('replace.title', { device: disk.name }));
    if (!res) return;
    state.step = 2;
    state.job = res.job || null;
    if (!state.job) {
      state.result = { ok: true, detail: T('replace.done_detail') };
      draw();
      if (onDone) onDone();
      return;
    }
    draw();
    await pollJob();
  };

  const pollJob = async () => {
    if (!win.isConnected || !state.job) return;
    try {
      const r = await screen.nas('tentaNasJobGetRequest', { jobId: state.job.jobId });
      state.job = r.job;
    } catch (e) {
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
    const ok = s === 'succeeded' || s === 'done';
    state.result = { ok, detail: ok ? T('replace.done_detail') : (state.job.error || T('jobs.status_' + s)) };
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
