// ===== File: modules/tentanas/pools.js — the Pools tab (n05): pool cards with capacity, protection and scrub state, the free-disk and spare shelf, and the two import dialogs (ZFS pool, Elastic Array) =====
//
// One card per pool answers the three questions an admin asks at a glance:
// is it healthy, how full is it, when was it last scrubbed. Everything
// deeper (topology, datasets, snapshots) lives on the detail screen.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import {
  T, sprite, POLL_POOLS_MS, ADMIN_TIMEOUT_MS,
  fmtDate, fmtIn, fmtBytes, fmtRatio, pct, healthClass, healthChip, errMessage, layoutLabel, stateChipHtml, fmtSchedule,
} from '/js/modules/tentanas/format.js';
import { setAttr, setText, patchHtml, patchKeyedList } from '/js/modules/tentanas/dom-patch.js';
import { openPoolWizard } from '/js/modules/tentanas/pool-wizard.js';
import { followResponse, warningHtml, openRetypeDialog } from '/js/modules/tentanas/dialogs.js';
import '/js/components/tf-window.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-menu.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-input.js';
import '/js/components/tf-checkbox.js';
import { elasticCardHtml } from '/js/modules/tentanas/elastic-detail.js';

export async function drawPools(screen, body) {
  const node = screen.currentNode();
  const host = document.createElement('div');
  body.replaceChildren(host);
  body = host;
  const isCurrent = () => !screen.disposed && host.isConnected && screen.currentNode()?.nodeId === node?.nodeId;
  body.innerHTML = `
    <div class="stack">
      <div class="section-card-head nas-pools-heading">
        <div class="title">${sprite('layers')} ${escapeHtml(T('pools.title', { node: node ? node.nodeName : '' }))} <tf-chip size="sm" status="neutral" id="nas-pools-count" label="0"></tf-chip></div>
        <div class="actions">
          <tf-button variant="secondary" icon="download" data-act="import" ${screen.isAdmin ? '' : 'disabled'}>${escapeHtml(T('pools.import'))}</tf-button>
          <tf-button variant="secondary" icon="download" data-act="import-array" ${screen.isAdmin ? '' : 'disabled'}>${escapeHtml(T('pools.import_array'))}</tf-button>
          <tf-button variant="primary" icon="plus" data-act="create" ${screen.isAdmin ? '' : 'disabled'}>${escapeHtml(T('pools.create'))}</tf-button>
        </div>
      </div>
      <div id="nas-pools-errors" class="stack"></div>
      <div id="nas-pools-list" class="stack"></div>
      <div class="section-card" id="nas-free-card" hidden>
        <div class="section-card-head">
          <div class="title">${sprite('cylinder')} ${escapeHtml(T('pools.free_disks'))} <tf-chip size="sm" status="neutral" id="nas-free-count" label="0"></tf-chip></div>
          <span class="hint" id="nas-free-hint"></span>
        </div>
        <div class="disk-cells" id="nas-free-cells"></div>
        <div class="explain-box mt-md">${T('pools.free_explain')}</div>
      </div>
    </div>`;

  const state = { pools: [], arrays: [], freeDisks: [], diskKinds: new Map(), errors: {}, completed: new Set(), epoch: 0, isCurrent };
  state.onCreated = ({ name, outcome }) => {
    if (!isCurrent()) return;
    if (outcome === 'job') screen.openArray(name);
    else if (outcome === 'approval') screen.switchTab('jobs');
    else screen.openArray(null);
  };
  const refresh = () => refreshPools(screen, body, state);
  // One polling chain per drawn tab; the action callbacks above refresh
  // without scheduling so they never add a second chain.
  const poll = async () => { await refresh(); if (isCurrent()) screen.later(poll, POLL_POOLS_MS); };
  const openWizard = () => {
    if (!isCurrent()) return;
    if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
    openPoolWizard(screen, { freeDisks: state.freeDisks, pools: state.pools, onDone: refresh, onCreated: state.onCreated, isCurrent });
  };

  body.querySelector('[data-act="create"]').addEventListener('click', openWizard);
  body.querySelector('[data-act="import"]').addEventListener('click', () => {
    if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
    openImportDialog(screen, refresh);
  });
  body.querySelector('[data-act="import-array"]').addEventListener('click', () => {
    if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
    openElasticImportDialog(screen, refresh);
  });

  const list = body.querySelector('#nas-pools-list');
  list.addEventListener('click', async (e) => {
    if (!isCurrent()) return;
    const elastic = e.target.closest('.pool-card[data-array]');
    if (elastic) { screen.openArray(elastic.dataset.array); return; }
    const card = e.target.closest('.pool-card[data-pool]');
    if (!card) return;
    const pool = state.pools.find((p) => p.name === card.dataset.pool);
    if (!pool) return;
    if (e.target.closest('tf-menu')) return;
    const btn = e.target.closest('[data-act]');
    if (btn) {
      e.stopPropagation();
      const act = btn.dataset.act;
      if (act === 'scrub') await scrubAction(screen, pool.name, 'start', refresh);
      else if (act === 'pause') await scrubAction(screen, pool.name, 'pause', refresh);
      else if (act === 'resume') await scrubAction(screen, pool.name, 'resume', refresh);
      else if (act === 'more') { const menu = card.querySelector('tf-menu'); menu.anchor = btn; menu.toggle(); }
      else screen.openPool(pool.name);
      return;
    }
    screen.openPool(pool.name);
  });
  list.addEventListener('action', async (e) => {
    const card = e.target.closest('.pool-card[data-pool]');
    const action = e.detail?.action;
    if (!card || !action) return;
    const name = card.dataset.pool;
    if (action === 'scrub-stop') await scrubAction(screen, name, 'stop', refresh);
    else if (action === 'create-empty') openWizard();
    else screen.openPool(name, action);
  });
  body.querySelector('#nas-free-cells').addEventListener('click', (e) => {
    const cell = e.target.closest('.disk-cell');
    if (!cell) return;
    if (cell.dataset.disk) screen.openDisk(cell.dataset.disk);
    else openWizard();
  });

  await poll();
}

/** Hot spares of every pool, flattened for the shelf: `{ disk, pool }`. */
export const spareDisks = (pools) => pools.flatMap((p) => (p.vdevs || []).filter((v) => v.role === 'spare').flatMap((v) => (v.disks || []).map((disk) => ({ disk, pool: p.name }))));

async function refreshPools(screen, body, state) {
  if (!state.isCurrent()) return;
  const epoch = ++state.epoch;
  await Promise.all([
    ['zfs', 'tentaNasPoolsListRequest', (res) => { state.pools = res.pools || []; }],
    ['elastic', 'tentaNasElasticArraysListRequest', (res) => { if (!Array.isArray(res.arrays)) throw new Error(T('elastic.bad_response')); state.arrays = res.arrays; }],
    ['capabilities', 'tentaNasElasticCapabilitiesRequest', (res) => { if (!Array.isArray(res.freeDisks)) throw new Error(T('elastic.bad_response')); state.freeDisks = res.freeDisks; }],
    ['disks', 'tentaNasDisksListRequest', (res) => { state.diskKinds = new Map((res.disks || []).map((d) => [d.diskId, d.kind])); }],
  ].map(async ([source, kind, accept]) => {
    try {
      const response = await screen.nas(kind, {});
      if (!state.isCurrent() || epoch !== state.epoch) return;
      accept(response);
      delete state.errors[source];
    } catch (error) {
      if (!state.isCurrent() || epoch !== state.epoch) return;
      state.errors[source] = errMessage(error);
      if (source === 'zfs') state.pools = [];
      if (source === 'elastic') state.arrays = [];
      if (source === 'capabilities') state.freeDisks = [];
    }
    state.completed.add(source);
    renderPools(screen, body, state);
  }));
}

function renderPools(screen, body, state) {
  if (!state.isCurrent()) return;
  const sources = { zfs: 'ZFS', elastic: 'Elastic Array', capabilities: T('elastic.free_inventory'), disks: T('elastic.disks') };
  patchHtml(body.querySelector('#nas-pools-errors'), Object.entries(state.errors).map(([source, error]) => `<tf-alert tone="danger" title="${escapeAttr(sources[source])}" message="${escapeAttr(error)}"></tf-alert>`).join(''));
  const partial = ['zfs', 'elastic'].some((key) => !state.completed.has(key) || state.errors[key]);
  setAttr(body.querySelector('#nas-pools-count'), 'label', String(state.pools.length + state.arrays.length) + (partial ? ' + ?' : ''));

  const list = body.querySelector('#nas-pools-list');
  if (!state.pools.length && !state.arrays.length && !partial) {
    const empty = `
      <tf-empty-state icon="layers" title="${escapeAttr(T('pools.empty_title'))}" message="${escapeAttr(state.freeDisks.length ? T('pools.empty_msg', { n: state.freeDisks.length }) : T('pools.empty_msg_no_disks'))}">
        ${state.freeDisks.length && screen.isAdmin ? `<tf-button variant="primary" icon="plus" data-act="create-empty">${escapeHtml(T('pools.create'))}</tf-button>` : ''}
      </tf-empty-state>`;
    if (patchHtml(list, empty)) {
      list.querySelector('[data-act="create-empty"]')?.addEventListener('click', () => {
        if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
        openPoolWizard(screen, { freeDisks: state.freeDisks, pools: state.pools, onDone: () => refreshPools(screen, body, state), onCreated: state.onCreated, isCurrent: state.isCurrent });
      });
    }
  } else {
    // The "still loading" line is part of the SAME patched string rather than
    // an insertAdjacentHTML afterwards: one host, one writer, or the cache
    // would describe markup that is not what is on screen.
    const loading = (!state.completed.has('zfs') || !state.completed.has('elastic'))
      ? `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`
      : '';
    patchHtml(list, state.pools.map((p) => poolCardHtml(p)).join('') + state.arrays.map(elasticCardHtml).join('') + loading);
  }

  const spares = spareDisks(state.pools);
  const freeCard = body.querySelector('#nas-free-card');
  freeCard.hidden = !state.freeDisks.length && !spares.length;
  if (freeCard.hidden) return;
  setAttr(body.querySelector('#nas-free-count'), 'label', String(state.freeDisks.length + spares.length));
  setText(body.querySelector('#nas-free-hint'), spares.length
    ? T('pools.spare_hint', { pool: [...new Set(spares.map((s) => s.pool))].join(', ') })
    : T('pools.free_hint', { n: state.freeDisks.length, size: fmtBytes(state.freeDisks.reduce((a, d) => a + (Number(d.sizeBytes) || 0), 0)) }));
  patchHtml(body.querySelector('#nas-free-cells'), `
    ${spares.map(({ disk, pool }) => {
    const kind = state.diskKinds.get(disk.diskId) || '';
    return `
      <div class="disk-cell spare" data-disk="${escapeAttr(disk.diskId || disk.name)}" title="${escapeAttr(T('pools.spare_title', { pool }))}">
        <span class="health-dot ${healthClass(disk.state === 'online' ? 'ok' : 'warning')}"></span>
        <div class="dc-main">
          <div class="dc-name"><span class="mono">${escapeHtml(disk.name)}</span></div>
          <div class="dc-sub">${escapeHtml(T('pools.spare_sub', { size: fmtBytes(disk.sizeBytes), pool }))}</div>
        </div>
        ${kind ? `<span class="disk-kind ${escapeAttr(kind)}">${escapeHtml(kind)}</span>` : ''}
      </div>`;
  }).join('')}
    ${state.freeDisks.map((d) => `
      <div class="disk-cell" data-disk="${escapeAttr(d.diskId)}" title="${escapeAttr(d.name)}">
        <span class="health-dot ${healthClass(d.health)}"></span>
        <div class="dc-main">
          <div class="dc-name"><span class="mono">${escapeHtml(d.name)}</span></div>
          <div class="dc-sub">${escapeHtml([fmtBytes(d.sizeBytes), d.model || '', T('pools.free_unused')].filter(Boolean).join(' · '))}</div>
        </div>
        <span class="disk-kind ${escapeAttr(d.kind)}">${escapeHtml(d.kind)}</span>
      </div>`).join('')}
    ${state.freeDisks.length ? `<div class="disk-cell empty" data-act="create">${sprite('plus')}&nbsp;${escapeHtml(T('pools.free_use'))}</div>` : ''}`);
}

/** "6×8 TB + special vdev (mirror) + SLOG + hot-spare · odporność: 2 dyski" — the one-line topology under the pool name. */
export function poolDescription(p) {
  const vdevs = p.vdevs || [];
  const dataDisks = vdevs.filter((v) => v.role === 'data').flatMap((v) => v.disks || []);
  const parts = [];
  if (dataDisks.length) parts.push(T('pools.desc_data', { n: dataDisks.length, size: fmtBytes(dataDisks[0].sizeBytes) }));
  const special = vdevs.find((v) => v.role === 'special');
  if (special) parts.push(T('pools.desc_special', { layout: layoutLabel(special.kind) }));
  if (vdevs.some((v) => v.role === 'log')) parts.push(T('pools.desc_log'));
  if (vdevs.some((v) => v.role === 'cache')) parts.push(T('pools.desc_cache'));
  if (vdevs.some((v) => v.role === 'spare')) parts.push(T('pools.desc_spare'));
  const topo = parts.join(' + ');
  return topo ? `${topo} · ${T('pools.desc_tolerance', { n: p.faultTolerance })}` : T('pools.desc_tolerance', { n: p.faultTolerance });
}

/** "6 danych + 2 special + spare" — the Dyski row of the card. */
function disksRowText(p) {
  const vdevs = p.vdevs || [];
  const count = (role) => vdevs.filter((v) => v.role === role).reduce((a, v) => a + (v.disks || []).length, 0);
  const parts = [T('pools.disks_data', { n: count('data') || p.dataDisks })];
  const special = count('special');
  if (special) parts.push(T('pools.disks_special', { n: special }));
  const log = count('log');
  if (log) parts.push(T('pools.disks_log', { n: log }));
  const cache = count('cache');
  if (cache) parts.push(T('pools.disks_cache', { n: cache }));
  if (count('spare')) parts.push(T('pools.disks_spare'));
  return parts.join(' + ');
}

// Raw size splits into used, free and parity/reserve: usable is what the
// layout leaves for data, the rest is redundancy the admin paid for.
export function poolCardHtml(p) {
  const raw = Number(p.sizeBytes) || 0;
  const usable = Number(p.usableBytes) || 0;
  const used = Number(p.usedBytes) || 0;
  const parity = Math.max(0, raw - usable);
  const free = Math.max(0, usable - used);
  const usedPct = pct(used, usable);
  const tone = usedPct > 90 ? 'err' : usedPct > 75 ? 'warn' : '';
  const health = healthChip(p.health);
  const scan = p.scan || {};
  const scanning = scan.status === 'running' || scan.status === 'paused';
  const scanChip = scanning
    ? `<tf-chip status="${scan.status === 'paused' ? 'warn' : 'accent'}" icon="${scan.kind === 'resilver' ? 'refresh' : 'shield'}" label="${escapeAttr(T('pools.scan_' + scan.kind, { pct: Math.round(Number(scan.progressPct) || 0) }))}"></tf-chip>`
    : '';
  const scanAction = scan.status === 'running'
    ? `<tf-button size="sm" variant="ghost" icon="pause" data-act="pause">${escapeHtml(T('pool.scrub_pause'))}</tf-button>`
    : scan.status === 'paused'
      ? `<tf-button size="sm" variant="ghost" icon="play" data-act="resume">${escapeHtml(T('pool.scrub_resume'))}</tf-button>`
      : `<tf-button size="sm" variant="ghost" icon="refresh" data-act="scrub">${escapeHtml(T('pools.scrub_now'))}</tf-button>`;
  return `
    <div class="pool-card" data-pool="${escapeAttr(p.name)}">
      <div class="pc-head">
        <div class="pc-ico">${sprite('layers')}</div>
        <div>
          <span class="pc-name">${escapeHtml(p.name)}</span>
          ${stateChipHtml(p.state)}
          <tf-chip status="accent" label="${escapeAttr(T('pools.layout_chip', { layout: layoutLabel(p.layout) }))}"></tf-chip>
          ${p.health !== 'ok' ? `<tf-chip status="${health.status}" dot label="${escapeAttr(health.label)}"></tf-chip>` : ''}
          ${p.encryption && p.encryption !== 'off' ? `<tf-chip status="info" icon="lock" label="${escapeAttr(T('pools.encrypted'))}"></tf-chip>` : ''}
          ${scanChip}
          <div class="pc-desc">${escapeHtml(poolDescription(p))}</div>
        </div>
        <div class="pc-actions">
          <tf-button size="sm" variant="secondary" icon="external-link" data-act="details">${escapeHtml(T('pools.details'))}</tf-button>
          ${scanAction}
          <tf-button size="sm" variant="ghost" icon="more" data-act="more" title="${escapeAttr(T('pools.more'))}"></tf-button>
          <tf-menu placement="bottom-end">
            ${scanning ? `<tf-menu-item action="scrub-stop" icon="stop">${escapeHtml(T('pool.scrub_stop'))}</tf-menu-item><tf-menu-divider></tf-menu-divider>` : ''}
            <tf-menu-item action="datasets" icon="folder">${escapeHtml(T('pool.tab_datasets'))}</tf-menu-item>
            <tf-menu-item action="snapshots" icon="clock">${escapeHtml(T('pool.tab_snapshots'))}</tf-menu-item>
          </tf-menu>
        </div>
      </div>
      ${p.healthReason ? `<div class="pc-reason ${healthClass(p.health)}">${sprite('alert')} ${escapeHtml(p.healthReason)}</div>` : ''}
      <div class="pc-body">
        <div>
          <div class="pc-cap"><span>${escapeHtml(T('pools.capacity', { raw: fmtBytes(raw) }))}</span><span class="v">${escapeHtml(T('pools.capacity_value', { used: fmtBytes(used), usable: fmtBytes(usable), pct: usedPct }))}</span></div>
          <div class="split-bar split-bar--3" title="${usedPct}%">
            <span class="${tone}" style="width:${pct(used, raw)}%"></span>
            <span class="free" style="width:${pct(free, raw)}%"></span>
            <span class="parity" style="width:${pct(parity, raw)}%"></span>
          </div>
          <div class="legend-rows">
            <div class="lr"><span class="sw ${tone || 'used'}"></span>${escapeHtml(T('pools.legend_used'))}<span class="v">${escapeHtml(fmtBytes(used))}</span></div>
            <div class="lr"><span class="sw free"></span>${escapeHtml(T('pools.legend_free'))}<span class="v">${escapeHtml(fmtBytes(free))}</span></div>
            <div class="lr"><span class="sw parity"></span>${escapeHtml(T('pools.legend_parity', { layout: p.layout }))}<span class="v">${escapeHtml(fmtBytes(parity))}</span></div>
          </div>
        </div>
        <div class="stat-rows">
          <div class="sr"><span class="k">${sprite('cylinder')}${escapeHtml(T('pools.row_disks'))}</span><span class="v">${escapeHtml(disksRowText(p))}</span></div>
          <div class="sr"><span class="k">${sprite('check')}${escapeHtml(T('pools.row_last_scrub'))}</span><span class="v">${p.lastScrubAt ? `${escapeHtml(fmtDate(p.lastScrubAt))} · <span class="${scan.errors ? 'num-err' : ''}">${escapeHtml(T('pools.scrub_errors', { n: Number(scan.errors) || 0 }))}</span>` : escapeHtml(T('pools.never'))}</span></div>
          <div class="sr"><span class="k">${sprite('clock')}${escapeHtml(T('pools.row_next_scrub'))}</span><span class="v"><span class="sched-pill">${sprite('clock')} ${escapeHtml(p.scrubSchedule ? fmtSchedule(p.scrubSchedule) : T('schedule.none'))}</span>${p.nextScrubAt ? ` <span class="text-3">${escapeHtml(fmtIn(p.nextScrubAt))}</span>` : ''}</span></div>
          <div class="sr"><span class="k">${sprite('zap')}${escapeHtml(T('pools.row_compression', { algo: p.compression || 'off' }))}</span><span class="v num-ok">${escapeHtml(fmtRatio(p.compressRatio))}</span></div>
        </div>
      </div>
    </div>`;
}

// Starting a scrub answers with a job (it runs for hours); pause/resume/stop
// answer with the refreshed pool. Both are admin actions.
export async function scrubAction(screen, name, action, onDone) {
  if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
  const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolScrubRequest', { name, action, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('pool.scrub_title', { name }));
  followResponse(screen, res, onDone, T('pool.scrub_' + action + '_done', { name }));
}

/**
 * `zpool trim` (§5.10). Same shape as the scrub action, because it is the same
 * kind of thing: a long pool operation that starts as a job and is then
 * suspended, resumed or cancelled.
 */
export async function trimAction(screen, name, action, onDone) {
  if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
  const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolTrimRequest', { name, action, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('pool.trim_title', { name }));
  followResponse(screen, res, onDone, T('pool.trim_' + action + '_done', { name }));
}

// Import (n05 modal): the scan needs root because zpool reads every disk
// label; the admin picks a pool, may rename it and may force-import one that
// was not exported cleanly (still holds the old host's claim).
export function openImportDialog(screen, onDone) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('import.title'));
  win.setAttribute('icon', 'download');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '640');
  win.setAttribute('min-width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  // `zpool import` names the labelled disks but says nothing about the media;
  // the node's disk inventory supplies the kind badge of each cell.
  const state = { pools: [], picked: null, newName: '', force: false, busy: false, scanning: true, kinds: new Map() };

  const poolHtml = (p) => `
    <div class="vdev-group ${state.picked && state.picked.guid === p.guid ? 'picked' : ''}" data-guid="${escapeAttr(p.guid)}">
      <div class="vg-head">
        <span class="vg-type">${escapeHtml(T('import.vg_type', { layout: layoutLabel(p.layout) }))}</span>
        <span class="mono fw-800">${escapeHtml(p.name)}</span>
        ${p.exportedCleanly
    ? `<tf-chip size="sm" status="ok" dot label="${escapeAttr(T('import.clean'))}"></tf-chip>`
    : `<tf-chip size="sm" status="warn" dot label="${escapeAttr(T('import.not_clean_chip'))}"></tf-chip>`}
        <span class="hint">${escapeHtml(T('import.row_sub', { n: (p.disks || []).length, state: p.state }))}</span>
      </div>
      <div class="disk-cells">
        ${(p.disks || []).map((d) => {
    const kind = state.kinds.get(d) || '';
    return `<div class="disk-cell"><span class="health-dot ok"></span><div class="dc-main"><div class="dc-name"><span class="mono">${escapeHtml(d)}</span></div><div class="dc-sub">${escapeHtml(T('import.disk_sub'))}</div></div>${kind ? `<span class="disk-kind ${escapeAttr(kind)}">${escapeHtml(kind)}</span>` : ''}</div>`;
  }).join('')}
      </div>
    </div>`;

  const draw = () => {
    const picked = state.picked;
    const intro = state.scanning
      ? `<div class="muted">${escapeHtml(T('import.scanning'))}</div>`
      : state.pools.length
        ? `<div class="text-2">${T('import.intro', { n: state.pools.length, found: `<b>${escapeHtml(T('import.intro_found', { n: state.pools.length }))}</b>` })}</div>`
        : `<div class="muted">${escapeHtml(T('import.none'))}</div>`;
    win.innerHTML = `
      <div slot="body" class="stack">
        <div class="row" style="align-items:center">
          <div style="flex:1">${intro}</div>
          <tf-button size="sm" variant="ghost" icon="refresh" data-act="rescan" ${state.scanning ? 'disabled' : ''}>${escapeHtml(T('import.rescan'))}</tf-button>
        </div>
        <div id="nas-import-list" class="stack">${state.pools.map(poolHtml).join('')}</div>
        ${picked ? `
          <div class="stack">
            ${picked.message ? `<div class="muted">${escapeHtml(picked.message)}</div>` : ''}
            <tf-input id="nas-import-name" label="${escapeAttr(T('import.new_name'))}" hint="${escapeAttr(T('import.new_name_hint'))}" placeholder="${escapeAttr(picked.name)}" value="${escapeAttr(state.newName)}" autocomplete="off" spellcheck="false"></tf-input>
            ${picked.exportedCleanly ? '' : `${warningHtml('danger', T('import.not_clean'))}<tf-checkbox id="nas-import-force" label="${escapeAttr(T('import.force'))}" ${state.force ? 'checked' : ''}></tf-checkbox>`}
          </div>` : ''}
        ${state.pools.length ? `<div class="wizard-warning info">${sprite('info')}<div>${T('import.explain')}</div></div>` : ''}
        <div class="num-err" id="nas-import-error" hidden></div>
      </div>
      <div slot="footer">
        <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
        <tf-button variant="primary" icon="download" data-action="confirm" ${picked && !state.busy ? '' : 'disabled'}>${escapeHtml(T('import.confirm'))}</tf-button>
      </div>`;
    win.querySelectorAll('.vdev-group[data-guid]').forEach((row) => row.addEventListener('click', () => {
      state.picked = state.pools.find((p) => p.guid === row.dataset.guid) || null;
      state.newName = '';
      state.force = false;
      draw();
    }));
    win.querySelector('#nas-import-name')?.addEventListener('input', (e) => { state.newName = e.target.value.trim(); });
    win.querySelector('#nas-import-force')?.addEventListener('change', (e) => { state.force = Boolean(e.detail?.checked); });
    win.querySelector('[data-act="rescan"]').addEventListener('click', scan);
  };

  const showError = (msg) => {
    const el = win.querySelector('#nas-import-error');
    if (el) { el.textContent = msg; el.hidden = !msg; }
  };

  const scan = async () => {
    state.scanning = true;
    draw();
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolImportScanRequest', { sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('import.title'));
    if (!win.isConnected) return;
    if (!res) { win.close(true); return; }
    const inventory = await screen.nas('tentaNasDisksListRequest', {}).catch(() => ({ disks: [] }));
    if (!win.isConnected) return;
    state.kinds = new Map((inventory.disks || []).map((d) => [d.name, d.kind]));
    state.scanning = false;
    state.pools = res.pools || [];
    state.picked = state.pools.length === 1 ? state.pools[0] : null;
    draw();
  };

  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (state.busy || !state.picked) return;
    state.busy = true;
    showError('');
    const picked = state.picked;
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolImportRequest', { guid: picked.guid, newName: state.newName, force: state.force, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('import.sudo_title', { name: picked.name }));
    state.busy = false;
    if (!res) { draw(); return; }
    win.close(true);
    followResponse(screen, res, onDone, T('import.done', { name: state.newName || picked.name }));
  });

  draw();
  document.body.appendChild(win);
  scan();
  return win;
}

// ---------------------------------------------------------------------------
// Elastic Array import (§5.3 recovery)
// ---------------------------------------------------------------------------
//
// WHY it is a second dialog and not a tab of the ZFS one: the two answer
// different questions from different sources. `zpool import` reads disk
// labels; this reads the root-only journals under /var/lib/tentanas and
// compares each member's recorded filesystem UUID against the live one.
//
// The scan is behind an explicit button rather than firing on open, because
// it needs sudo: opening a dialog must never be what makes a node ask for a
// root password.
export function openElasticImportDialog(screen, onDone) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('elastic_import.title'));
  win.setAttribute('icon', 'download');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '680');
  win.setAttribute('min-width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  // `candidates: null` is "nothing has been scanned yet" and is NOT an empty
  // result: the two say different things and the dialog must not print "no
  // array found" before it has looked.
  const state = { candidates: null, scanning: false, error: '' };

  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="explain-box">${T('elastic_import.explain')}</div>
      <div class="row" style="align-items:center">
        <div style="flex:1"><span class="muted" id="nas-eimport-status"></span></div>
        <tf-button size="sm" variant="secondary" icon="refresh" data-act="scan">${escapeHtml(T('elastic_import.scan'))}</tf-button>
      </div>
      <div id="nas-eimport-list" class="stack"></div>
      <div class="num-err" id="nas-eimport-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
    </div>`;
  const statusEl = win.querySelector('#nas-eimport-status');
  const listHost = win.querySelector('#nas-eimport-list');
  const errEl = win.querySelector('#nas-eimport-error');
  const scanBtn = win.querySelector('[data-act="scan"]');

  const CHIP = {
    importable: ['ok', 'status_importable'],
    incomplete: ['warn', 'status_incomplete'],
    already_known: ['neutral', 'status_already_known'],
    unreadable: ['err', 'status_unreadable'],
  };

  const candidateHtml = (c) => {
    const [tone, label] = CHIP[c.status] || CHIP.incomplete;
    const total = (c.disksMatched || 0) + (c.disksMissing || []).length + (c.disksReused || []).length;
    return `
      <div class="vdev-group ${c.status === 'importable' ? 'picked' : ''}" data-array="${escapeAttr(c.arrayId)}" title="${escapeAttr(c.detail || '')}">
        <div class="vg-head">
          <span class="vg-type">${escapeHtml(T('elastic_import.vg_type', { fs: c.filesystem || '' }))}</span>
          <span class="mono fw-800">${escapeHtml(c.name)}</span>
          <tf-chip size="sm" status="${escapeAttr(tone)}" dot label="${escapeAttr(T('elastic_import.' + label))}"></tf-chip>
          <span class="hint">${escapeHtml(T('elastic_import.counts', { data: c.dataDisks || 0, parity: c.parityDisks || 0, cache: c.cacheDisks || 0 }))}</span>
        </div>
        <div class="hint">${escapeHtml(T('elastic_import.owner', { owner: `${c.ownerOrgId || '—'}/${c.ownerAddonId || '—'}` }))}</div>
        <div class="hint">${escapeHtml(T('elastic_import.matched', { n: c.disksMatched || 0, total }))}${c.unionMounted ? ` · ${escapeHtml(T('elastic_import.union_mounted'))}` : ''}</div>
        ${(c.disksMissing || []).length ? `<div class="num-err">${escapeHtml(T('elastic_import.missing', { disks: c.disksMissing.join(', ') }))}</div>` : ''}
        ${(c.disksReused || []).length ? `<div class="num-err">${escapeHtml(T('elastic_import.reused', { disks: c.disksReused.join(', ') }))}</div>` : ''}
        ${c.status === 'unreadable' ? `<div class="num-err">${escapeHtml(c.detail || '')}</div>` : ''}
        <div class="row">
          <tf-button size="sm" variant="primary" icon="download" data-act="adopt" ${c.status === 'importable' ? '' : 'disabled'}>${escapeHtml(T('elastic_import.adopt'))}</tf-button>
        </div>
      </div>`;
  };

  // Only what changed: the status line, the scan button's state and the rows
  // whose own markup differs. A row an admin is reading keeps its node.
  const paint = () => {
    setText(statusEl, state.scanning
      ? T('elastic_import.scanning')
      : state.candidates === null
        ? T('elastic_import.idle')
        : state.candidates.length
          ? T('elastic_import.found', { n: state.candidates.length })
          : T('elastic_import.none'));
    setAttr(scanBtn, 'disabled', state.scanning);
    patchKeyedList(listHost, (state.candidates || []).map((c) => ({ key: c.arrayId, html: candidateHtml(c) })));
    setText(errEl, state.error);
    errEl.hidden = !state.error;
  };

  const scan = async () => {
    if (state.scanning) return;
    state.scanning = true;
    state.error = '';
    paint();
    try {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasElasticArrayImportScanRequest', { sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('elastic_import.title'));
      if (!win.isConnected) return;
      // `null` is a cancelled sudo prompt: nothing was scanned, so the dialog
      // keeps saying it has not looked yet.
      if (res) state.candidates = res.candidates || [];
    } catch (err) {
      state.error = errMessage(err);
    } finally {
      state.scanning = false;
      if (win.isConnected) paint();
    }
  };

  // Retyping the name, exactly as destroying a pool demands it: an adoption
  // re-owns storage that belongs to another identity, and the journal owner
  // is on the dialog so nobody re-owns somebody else's array by accident.
  const openAdopt = (c) => openRetypeDialog({
    title: T('elastic_import.adopt_title', { name: c.name }),
    icon: 'download',
    confirmIcon: 'download',
    name: c.name,
    bodyHtml: `
      ${warningHtml('danger', T('elastic_import.reown_warning', { owner: `${c.ownerOrgId || '—'}/${c.ownerAddonId || '—'}` }))}
      <div class="explain-box">${T('elastic_import.adopt_explain', { n: c.disksMatched || 0, name: escapeHtml(c.name) })}</div>`,
    retypeLabel: `${escapeHtml(T('elastic_import.retype'))} <span class="mono num-err">${escapeHtml(c.name)}</span>`,
    confirmLabel: T('elastic_import.confirm'),
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasElasticArrayImportRequest', { arrayId: c.arrayId, confirmName: c.name, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('elastic_import.adopt_title', { name: c.name }));
      if (res === null) return false;
      win.close(true);
      followResponse(screen, res, onDone, T('elastic_import.done', { name: c.name }));
      return true;
    },
  });

  scanBtn.addEventListener('click', scan);
  // One delegated listener for every row, so a re-scan that replaces a card
  // never has to re-wire anything.
  listHost.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-act="adopt"]');
    if (!btn || btn.hasAttribute('disabled')) return;
    const row = btn.closest('[data-array]');
    const candidate = (state.candidates || []).find((c) => c.arrayId === row?.dataset.array);
    if (candidate) openAdopt(candidate);
  });
  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'cancel') win.close(true);
  });

  paint();
  document.body.appendChild(win);
  return win;
}
