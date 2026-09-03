// ===== File: modules/tentanas/pools.js — the Pools tab (n05): pool cards with capacity, protection and scrub state, the free-disk and spare shelf, and the import dialog =====
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
import { openPoolWizard } from '/js/modules/tentanas/pool-wizard.js';
import { followResponse, warningHtml } from '/js/modules/tentanas/dialogs.js';
import '/js/components/tf-window.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-menu.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-input.js';
import '/js/components/tf-checkbox.js';

export async function drawPools(screen, body) {
  const node = screen.currentNode();
  body.innerHTML = `
    <div class="stack">
      <div class="section-card-head">
        <div class="title">${sprite('layers')} ${escapeHtml(T('pools.title', { node: node ? node.nodeName : '' }))} <tf-chip size="sm" status="neutral" id="nas-pools-count" label="0"></tf-chip></div>
        <span class="hint" id="nas-pools-sub">${escapeHtml(I18n.t('common.loading'))}</span>
        <div class="actions">
          <tf-button variant="secondary" icon="download" data-act="import">${escapeHtml(T('pools.import'))}</tf-button>
          <tf-button variant="primary" icon="plus" data-act="create">${escapeHtml(T('pools.create'))}</tf-button>
        </div>
      </div>
      <div id="nas-pools-list"></div>
      <div class="section-card" id="nas-free-card" hidden>
        <div class="section-card-head">
          <div class="title">${sprite('cylinder')} ${escapeHtml(T('pools.free_disks'))} <tf-chip size="sm" status="neutral" id="nas-free-count" label="0"></tf-chip></div>
          <span class="hint" id="nas-free-hint"></span>
        </div>
        <div class="disk-cells" id="nas-free-cells"></div>
      </div>
    </div>`;

  const state = { pools: [], freeDisks: [] };
  const refresh = () => refreshPools(screen, body, state);
  // One polling chain per drawn tab; the action callbacks above refresh
  // without scheduling so they never add a second chain.
  const poll = async () => { await refresh(); if (!screen.disposed && body.isConnected) screen.later(poll, POLL_POOLS_MS); };
  const openWizard = () => {
    if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
    openPoolWizard(screen, { freeDisks: state.freeDisks, pools: state.pools, onDone: refresh });
  };

  body.querySelector('[data-act="create"]').addEventListener('click', openWizard);
  body.querySelector('[data-act="import"]').addEventListener('click', () => {
    if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
    openImportDialog(screen, refresh);
  });

  const list = body.querySelector('#nas-pools-list');
  list.addEventListener('click', async (e) => {
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
  if (screen.disposed || !body.isConnected) return;
  let res;
  try {
    res = await screen.nas('tentaNasPoolsListRequest', {});
  } catch (e) {
    if (screen.disposed || !body.isConnected) return;
    const sub = body.querySelector('#nas-pools-sub');
    if (sub) sub.textContent = errMessage(e);
    return;
  }
  if (screen.disposed || !body.isConnected) return;
  state.pools = res.pools || [];
  state.freeDisks = res.freeDisks || [];

  const usable = state.pools.reduce((a, p) => a + (Number(p.usableBytes) || 0), 0);
  const used = state.pools.reduce((a, p) => a + (Number(p.usedBytes) || 0), 0);
  body.querySelector('#nas-pools-count').setAttribute('label', String(state.pools.length));
  body.querySelector('#nas-pools-sub').textContent = state.pools.length
    ? T('pools.sub', { used: fmtBytes(used), usable: fmtBytes(usable) })
    : T('pools.sub_empty');

  const list = body.querySelector('#nas-pools-list');
  if (!state.pools.length) {
    list.innerHTML = `
      <tf-empty-state icon="layers" title="${escapeAttr(T('pools.empty_title'))}" message="${escapeAttr(state.freeDisks.length ? T('pools.empty_msg', { n: state.freeDisks.length }) : T('pools.empty_msg_no_disks'))}">
        ${state.freeDisks.length ? `<tf-button variant="primary" icon="plus" data-act="create-empty">${escapeHtml(T('pools.create'))}</tf-button>` : ''}
      </tf-empty-state>`;
    list.querySelector('[data-act="create-empty"]')?.addEventListener('click', () => {
      if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
      openPoolWizard(screen, { freeDisks: state.freeDisks, pools: state.pools, onDone: () => refreshPools(screen, body, state) });
    });
  } else {
    list.innerHTML = state.pools.map((p) => poolCardHtml(p)).join('');
  }

  const spares = spareDisks(state.pools);
  const freeCard = body.querySelector('#nas-free-card');
  freeCard.hidden = !state.freeDisks.length && !spares.length;
  if (freeCard.hidden) return;
  body.querySelector('#nas-free-count').setAttribute('label', String(state.freeDisks.length + spares.length));
  body.querySelector('#nas-free-hint').textContent = spares.length
    ? T('pools.spare_hint', { pool: [...new Set(spares.map((s) => s.pool))].join(', ') })
    : T('pools.free_hint', { n: state.freeDisks.length, size: fmtBytes(state.freeDisks.reduce((a, d) => a + (Number(d.sizeBytes) || 0), 0)) });
  body.querySelector('#nas-free-cells').innerHTML = `
    ${spares.map(({ disk, pool }) => `
      <div class="disk-cell spare" data-disk="${escapeAttr(disk.diskId || disk.name)}" title="${escapeAttr(T('pools.spare_title', { pool }))}">
        <span class="health-dot ${healthClass(disk.state === 'online' ? 'ok' : 'warning')}"></span>
        <div class="dc-main">
          <div class="dc-name"><span class="mono">${escapeHtml(disk.name)}</span></div>
          <div class="dc-sub">${escapeHtml(T('pools.spare_sub', { size: fmtBytes(disk.sizeBytes), pool }))}</div>
        </div>
      </div>`).join('')}
    ${state.freeDisks.map((d) => `
      <div class="disk-cell" data-disk="${escapeAttr(d.diskId)}" title="${escapeAttr(d.name)}">
        <span class="health-dot ${healthClass(d.health)}"></span>
        <div class="dc-main">
          <div class="dc-name"><span class="mono">${escapeHtml(d.name)}</span></div>
          <div class="dc-sub">${escapeHtml([fmtBytes(d.sizeBytes), d.model || '', T('pools.free_unused')].filter(Boolean).join(' · '))}</div>
        </div>
        <span class="disk-kind ${escapeAttr(d.kind)}">${escapeHtml(d.kind)}</span>
      </div>`).join('')}
    ${state.freeDisks.length ? `<div class="disk-cell empty" data-act="create">${sprite('plus')}&nbsp;${escapeHtml(T('pools.free_use'))}</div>` : ''}`;
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
            <tf-menu-item action="properties" icon="settings">${escapeHtml(T('pool.tab_properties'))}</tf-menu-item>
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
  const state = { pools: [], picked: null, newName: '', force: false, busy: false, scanning: true };

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
        ${(p.disks || []).map((d) => `<div class="disk-cell"><span class="health-dot ok"></span><div class="dc-main"><div class="dc-name"><span class="mono">${escapeHtml(d)}</span></div><div class="dc-sub">${escapeHtml(T('import.disk_sub'))}</div></div></div>`).join('')}
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
