// ===== File: modules/tentanas/pools.js — the Pools tab (n05): pool cards with capacity, protection and scrub state, the free-disk shelf, and the import dialog =====
//
// One card per pool answers the three questions an admin asks at a glance:
// is it healthy, how full is it, when was it last scrubbed. Everything
// deeper (topology, datasets, snapshots) lives on the detail screen.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import {
  T, sprite, POLL_POOLS_MS, ADMIN_TIMEOUT_MS,
  fmtAgo, fmtIn, fmtBytes, fmtRatio, pct, healthClass, healthChip, errMessage, layoutLabel, stateChipHtml, fmtSchedule,
} from '/js/modules/tentanas/format.js';
import { openPoolWizard } from '/js/modules/tentanas/pool-wizard.js';
import { followResponse, warningHtml } from '/js/modules/tentanas/dialogs.js';
import '/js/components/tf-window.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-option-row.js';
import '/js/components/tf-input.js';
import '/js/components/tf-checkbox.js';

export async function drawPools(screen, body) {
  body.innerHTML = `
    <div class="stack">
      <div class="page-head">
        <div>
          <h1>${sprite('layers')} ${escapeHtml(T('pools.title'))}</h1>
          <div class="sub" id="nas-pools-sub">${escapeHtml(I18n.t('common.loading'))}</div>
        </div>
        <div class="actions">
          <tf-button variant="secondary" icon="download" data-act="import">${escapeHtml(T('pools.import'))}</tf-button>
          <tf-button variant="primary" icon="plus" data-act="create">${escapeHtml(T('pools.create'))}</tf-button>
        </div>
      </div>
      <div id="nas-pools-list"></div>
      <div class="section-card" id="nas-free-card" hidden>
        <div class="section-card-head"><div class="title">${sprite('cylinder')} ${escapeHtml(T('pools.free_disks'))}</div><span class="hint" id="nas-free-hint"></span></div>
        <div class="disk-cells" id="nas-free-cells"></div>
      </div>
    </div>`;

  const state = { pools: [], freeDisks: [] };
  const refresh = () => refreshPools(screen, body, state);
  // One polling chain per drawn tab; the action callbacks above refresh
  // without scheduling so they never add a second chain.
  const poll = async () => { await refresh(); if (!screen.disposed && body.isConnected) screen.later(poll, POLL_POOLS_MS); };

  body.querySelector('[data-act="create"]').addEventListener('click', () => {
    if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
    openPoolWizard(screen, { freeDisks: state.freeDisks, onDone: refresh });
  });
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
    const btn = e.target.closest('[data-act]');
    if (btn) {
      e.stopPropagation();
      const act = btn.dataset.act;
      if (act === 'scrub') await scrubAction(screen, pool.name, 'start', refresh);
      else if (act === 'pause') await scrubAction(screen, pool.name, 'pause', refresh);
      else if (act === 'resume') await scrubAction(screen, pool.name, 'resume', refresh);
      else screen.openPool(pool.name);
      return;
    }
    screen.openPool(pool.name);
  });
  body.querySelector('#nas-free-cells').addEventListener('click', () => {
    if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
    openPoolWizard(screen, { freeDisks: state.freeDisks, onDone: refresh });
  });

  await poll();
}

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
  body.querySelector('#nas-pools-sub').textContent = state.pools.length
    ? T('pools.sub', { n: state.pools.length, used: fmtBytes(used), usable: fmtBytes(usable) })
    : T('pools.sub_empty');

  const list = body.querySelector('#nas-pools-list');
  if (!state.pools.length) {
    list.innerHTML = `
      <tf-empty-state icon="layers" title="${escapeAttr(T('pools.empty_title'))}" message="${escapeAttr(state.freeDisks.length ? T('pools.empty_msg', { n: state.freeDisks.length }) : T('pools.empty_msg_no_disks'))}">
        ${state.freeDisks.length ? `<tf-button variant="primary" icon="plus" data-act="create-empty">${escapeHtml(T('pools.create'))}</tf-button>` : ''}
      </tf-empty-state>`;
    list.querySelector('[data-act="create-empty"]')?.addEventListener('click', () => {
      if (!screen.isAdmin) { toast(T('elevation.admin_only'), 'warning'); return; }
      openPoolWizard(screen, { freeDisks: state.freeDisks, onDone: () => refreshPools(screen, body, state) });
    });
  } else {
    list.innerHTML = state.pools.map((p) => poolCardHtml(p)).join('');
  }

  const freeCard = body.querySelector('#nas-free-card');
  freeCard.hidden = !state.freeDisks.length;
  if (state.freeDisks.length) {
    body.querySelector('#nas-free-hint').textContent = T('pools.free_hint', { n: state.freeDisks.length, size: fmtBytes(state.freeDisks.reduce((a, d) => a + (Number(d.sizeBytes) || 0), 0)) });
    body.querySelector('#nas-free-cells').innerHTML = state.freeDisks.map((d) => `
      <div class="disk-cell empty" data-disk="${escapeAttr(d.diskId)}" title="${escapeAttr(T('pools.free_use'))}">
        <div class="dc-main">
          <div class="dc-name"><span class="health-dot ${healthClass(d.health)}"></span><span class="mono">${escapeHtml(d.name)}</span><span class="disk-kind ${escapeAttr(d.kind)}">${escapeHtml(d.kind)}</span></div>
          <div class="dc-sub">${escapeHtml([fmtBytes(d.sizeBytes), d.model || ''].filter(Boolean).join(' · '))}</div>
        </div>
        ${sprite('plus')}
      </div>`).join('');
  }
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
  const scanActions = scan.status === 'running'
    ? `<tf-button size="sm" variant="ghost" icon="pause" data-act="pause">${escapeHtml(T('pool.scrub_pause'))}</tf-button>`
    : scan.status === 'paused'
      ? `<tf-button size="sm" variant="ghost" icon="play" data-act="resume">${escapeHtml(T('pool.scrub_resume'))}</tf-button>`
      : `<tf-button size="sm" variant="ghost" icon="shield" data-act="scrub">${escapeHtml(T('pool.scrub_now'))}</tf-button>`;
  const disksText = T('pools.disks_value', { n: p.dataDisks, ft: p.faultTolerance });
  return `
    <div class="pool-card" data-pool="${escapeAttr(p.name)}">
      <div class="pc-head">
        <div class="pc-ico">${sprite('layers')}</div>
        <div class="pc-name">${escapeHtml(p.name)}</div>
        <tf-chip status="${health.status}" dot label="${escapeAttr(health.label)}"></tf-chip>
        ${stateChipHtml(p.state)}
        <tf-chip status="info" label="${escapeAttr(layoutLabel(p.layout))} · ${p.dataDisks}×"></tf-chip>
        ${p.encryption && p.encryption !== 'off' ? `<tf-chip status="info" icon="lock" label="${escapeAttr(T('pools.encrypted'))}"></tf-chip>` : ''}
        ${scanChip}
        <div class="pc-actions">
          ${scanActions}
          <tf-button size="sm" variant="ghost" icon="chevron-right" data-act="details">${escapeHtml(T('pools.details'))}</tf-button>
        </div>
      </div>
      ${p.healthReason ? `<div class="pc-reason ${healthClass(p.health)}">${sprite('alert')} ${escapeHtml(p.healthReason)}</div>` : ''}
      <div class="pc-body">
        <div>
          <div class="pc-cap"><span>${escapeHtml(T('pools.capacity'))}</span><span class="v">${escapeHtml(fmtBytes(used))} / ${escapeHtml(fmtBytes(usable))} · ${usedPct}%</span></div>
          <div class="split-bar split-bar--3" title="${usedPct}%">
            <span class="${tone}" style="width:${pct(used, raw)}%"></span>
            <span class="free" style="width:${pct(free, raw)}%"></span>
            <span class="parity" style="width:${pct(parity, raw)}%"></span>
          </div>
          <div class="legend-rows">
            <div class="lr"><span class="sw ${tone || 'used'}"></span>${escapeHtml(T('pools.legend_used'))}<span class="v">${escapeHtml(fmtBytes(used))}</span></div>
            <div class="lr"><span class="sw free"></span>${escapeHtml(T('pools.legend_free'))}<span class="v">${escapeHtml(fmtBytes(free))}</span></div>
            <div class="lr"><span class="sw parity"></span>${escapeHtml(T('pools.legend_parity'))}<span class="v">${escapeHtml(fmtBytes(parity))}</span></div>
          </div>
        </div>
        <div class="stat-rows">
          <div class="sr"><span class="k">${escapeHtml(T('pools.row_disks'))}</span><span class="v">${escapeHtml(disksText)}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('pools.row_last_scrub'))}</span><span class="v">${escapeHtml(p.lastScrubAt ? fmtAgo(p.lastScrubAt) : T('pools.never'))}${scan.errors ? ` <span class="num-err">${escapeHtml(T('pools.scrub_errors', { n: scan.errors }))}</span>` : ''}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('pools.row_next_scrub'))}</span><span class="v"><span class="sched-pill">${sprite('clock')} ${escapeHtml(p.scrubSchedule ? fmtSchedule(p.scrubSchedule) : T('schedule.none'))}</span>${p.nextScrubAt ? ` <span class="text-3">${escapeHtml(fmtIn(p.nextScrubAt))}</span>` : ''}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('pools.row_compression'))}</span><span class="v mono">${escapeHtml(p.compression || 'off')} · ${escapeHtml(fmtRatio(p.compressRatio))}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('pools.row_datasets'))}</span><span class="v">${escapeHtml(T('pools.datasets_value', { d: p.datasetCount, s: p.snapshotCount }))}</span></div>
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

// Import (n18): the scan needs root because zpool reads every disk label;
// the admin picks a pool, may rename it and may force-import one that was
// not exported cleanly (still holds the old host's claim).
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
  const state = { pools: [], picked: null, newName: '', force: false, busy: false };

  const draw = () => {
    const picked = state.picked;
    win.innerHTML = `
      <div slot="body" class="stack">
        <div class="explain-box">${escapeHtml(T('import.explain'))}</div>
        <div id="nas-import-list">${state.pools.length ? state.pools.map((p) => `
          <tf-option-row value="${escapeAttr(p.guid)}" label="${escapeAttr(p.name)}" sub="${escapeAttr(T('import.row_sub', { layout: layoutLabel(p.layout), n: (p.disks || []).length, state: p.state }))}" marker="${p.exportedCleanly ? 'ok' : 'warn'}" ${picked && picked.guid === p.guid ? 'selected' : ''}></tf-option-row>`).join('')
          : `<div class="muted">${escapeHtml(T('import.none'))}</div>`}</div>
        ${picked ? `
          <div class="stack">
            <div class="kv-inline"><span class="k">${escapeHtml(T('import.disks'))}</span><span class="v mono">${escapeHtml((picked.disks || []).join(', ') || '—')}</span></div>
            ${picked.message ? `<div class="muted">${escapeHtml(picked.message)}</div>` : ''}
            <tf-input id="nas-import-name" label="${escapeAttr(T('import.new_name'))}" hint="${escapeAttr(T('import.new_name_hint'))}" placeholder="${escapeAttr(picked.name)}" value="${escapeAttr(state.newName)}" autocomplete="off" spellcheck="false"></tf-input>
            ${picked.exportedCleanly ? '' : warningHtml('danger', T('import.not_clean'))}
            <tf-checkbox id="nas-import-force" label="${escapeAttr(T('import.force'))}" ${state.force ? 'checked' : ''}></tf-checkbox>
          </div>` : ''}
        <div class="num-err" id="nas-import-error" hidden></div>
      </div>
      <div slot="footer">
        <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
        <tf-button variant="ghost" icon="refresh" data-act="rescan">${escapeHtml(T('import.rescan'))}</tf-button>
        <tf-button variant="primary" icon="download" data-action="confirm" ${picked && !state.busy ? '' : 'disabled'}>${escapeHtml(T('import.confirm'))}</tf-button>
      </div>`;
    win.querySelectorAll('tf-option-row').forEach((row) => row.addEventListener('click', () => {
      state.picked = state.pools.find((p) => p.guid === row.getAttribute('value')) || null;
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
    const list = win.querySelector('#nas-import-list');
    if (list) list.innerHTML = `<div class="muted">${escapeHtml(T('import.scanning'))}</div>`;
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolImportScanRequest', { sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('import.title'));
    if (!win.isConnected) return;
    if (!res) { win.close(true); return; }
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
