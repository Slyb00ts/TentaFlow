// ===== File: modules/tentanas/snapshots.js — the Snapshots inner tab of a pool (n10): schedules with GFS retention, the snapshot list with bulk delete, rollback, clone, snapshot-now =====
//
// Rollback is the one action here that can silently discard data: ZFS
// refuses to roll back past newer snapshots unless they are destroyed, so
// the dialog lists those snapshots by name and only sends `destroyNewer`
// when there are some and the admin retyped the snapshot name.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import {
  T, sprite, ADMIN_TIMEOUT_MS, parseServerTs,
  fmtDate, fmtAgo, fmtBytes, errMessage, fmtSchedule,
} from '/js/modules/tentanas/format.js';
import { scheduleFieldsHtml, wireScheduleFields, readScheduleFields, normalizeSchedule } from '/js/modules/tentanas/schedule-editor.js';
import { openRetypeDialog, followResponse, pathCrumbsHtml, wirePathCrumbs } from '/js/modules/tentanas/dialogs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-select.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-window.js';
import '/js/components/tf-input.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-checkbox.js';

const LIST_LIMIT = 500;
const KEEP_KEYS = ['keepFrequent', 'keepHourly', 'keepDaily', 'keepWeekly', 'keepMonthly'];
const DEFAULT_KEEP = { keepFrequent: 0, keepHourly: 24, keepDaily: 7, keepWeekly: 4, keepMonthly: 3 };

export async function drawSnapshots(screen, host, { pool, datasets = [], onChange = null }) {
  const admin = screen.isAdmin;
  host.innerHTML = `
    <div class="stack">
      <div class="grid-2">
        <div class="section-card" id="nas-snap-schedule"></div>
        <div class="section-card" id="nas-snap-smb"></div>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${escapeHtml(T('snapshots.title'))} <tf-chip size="sm" id="nas-snap-total" label="0"></tf-chip></div>
          <div class="actions">
            <tf-select id="nas-snap-dataset"></tf-select>
            <tf-searchbox id="nas-snap-search" placeholder="${escapeAttr(T('snapshots.search'))}" debounce="150"></tf-searchbox>
            ${admin ? `<tf-button variant="primary" size="sm" icon="save" data-act="snap-now">${escapeHtml(T('snapshots.now'))}</tf-button>` : ''}
          </div>
        </div>
        <tf-filter-chips id="nas-snap-filters" class="mb-sm"></tf-filter-chips>
        <tf-table id="nas-snap-table" empty-message="${escapeAttr(T('snapshots.none'))}">
          <tf-column key="name" label="${escapeAttr(T('snapshots.col_name'))}" renderer="html" fill sortable></tf-column>
          <tf-column key="created" label="${escapeAttr(T('snapshots.col_created'))}" renderer="html" nowrap sortable></tf-column>
          <tf-column key="used" label="${escapeAttr(T('snapshots.col_used'))}" renderer="html" nowrap hide-below="900"></tf-column>
          <tf-column key="origin" label="${escapeAttr(T('snapshots.col_type'))}" renderer="html" nowrap></tf-column>
        </tf-table>
      </div>
    </div>`;

  const state = { pool, dataset: '', query: '', filter: 'all', snapshots: [], schedules: [], shares: [], total: 0, totalUsed: 0 };
  const table = host.querySelector('#nas-snap-table');
  const dsSel = host.querySelector('#nas-snap-dataset');
  const totalCount = datasets.reduce((n, d) => n + (Number(d.snapshotCount) || 0), 0);
  dsSel.setOptions([
    { value: '', label: T('snapshots.all_datasets', { n: totalCount }) },
    ...datasets.map((d) => ({ value: d.name, label: T('snapshots.dataset_option', { name: d.name, n: Number(d.snapshotCount) || 0 }) })),
  ], '');
  if (screen.dataset && datasets.some((d) => d.name === screen.dataset)) { state.dataset = screen.dataset; dsSel.value = screen.dataset; }
  dsSel.addEventListener('change', (e) => { state.dataset = e.detail.value; reloadList(); paintCards(); });
  const filters = host.querySelector('#nas-snap-filters');
  filters.addEventListener('change', (e) => { state.filter = e.detail.id; reloadList(); });
  host.querySelector('#nas-snap-search').addEventListener('search', (e) => { state.query = (e.detail.value || '').trim().toLowerCase(); applyRows(); });

  const reloadSchedules = async () => {
    try {
      const [sc, sh] = await Promise.all([
        screen.nas('tentaNasSnapshotSchedulesListRequest', {}),
        screen.nas('tentaNasSharesListRequest', {}),
      ]);
      state.schedules = (sc.schedules || []).filter((s) => s.dataset === pool || s.dataset.startsWith(pool + '/'));
      state.shares = sh.shares || [];
    } catch (e) {
      toast(errMessage(e), 'error');
      return;
    }
    if (!host.isConnected) return;
    paintCards();
  };

  // Both cards speak about the dataset picked in the list filter (the pool
  // root when "all datasets" is shown).
  const focusName = () => state.dataset || pool;
  const focusDataset = () => datasets.find((d) => d.name === focusName()) || { name: focusName() };

  const paintCards = () => {
    const name = focusName();
    const sched = state.schedules.find((s) => s.dataset === name) || null;
    const others = state.schedules.filter((s) => s.dataset !== name).length;
    const latest = state.snapshots.filter((s) => s.dataset === name).slice().sort((a, b) => (parseServerTs(b.createdAt)?.getTime() ?? 0) - (parseServerTs(a.createdAt)?.getTime() ?? 0))[0];
    const el = host.querySelector('#nas-snap-schedule');
    el.innerHTML = `
      <div class="section-card-head">
        <div class="title">${escapeHtml(T('snapshots.schedule_title'))}</div>
        <div class="actions">${admin ? `
          ${sched ? `<tf-button variant="ghost" size="sm" tone="critical" icon="trash" data-act="delete-schedule" title="${escapeAttr(T('snapshots.schedule_delete'))}"></tf-button>` : ''}
          <tf-button variant="secondary" size="sm" icon="${sched ? 'edit' : 'plus'}" data-act="edit-schedule">${escapeHtml(sched ? T('snapshots.schedule_edit') : T('snapshots.schedule_add'))}</tf-button>` : ''}</div>
      </div>
      <div class="stat-rows">
        <div class="sr"><span class="k">${escapeHtml(T('schedule.every_label'))}</span><span class="v">${sched ? `<span class="sched-pill">${sprite('clock')} ${escapeHtml(fmtSchedule(sched.schedule))}</span>${sched.enabled ? '' : ` <tf-chip size="sm" status="warn" label="${escapeAttr(T('schedule.off'))}"></tf-chip>`}` : escapeHtml(T('schedule.none'))}</span></div>
        <div class="sr"><span class="k">${escapeHtml(T('snapshots.retention'))}</span><span class="v">${escapeHtml(sched ? keepSummary(sched) : '—')}</span></div>
        <div class="sr"><span class="k">${escapeHtml(T('snapshots.last_snapshot'))}</span><span class="v">${latest ? `${escapeHtml(fmtAgo(latest.createdAt))} <span class="text-3">(${escapeHtml(latest.shortName)})</span>` : '—'}</span></div>
        <div class="sr"><span class="k">${escapeHtml(T('snapshots.space_used'))}</span><span class="v">${escapeHtml(fmtBytes(state.totalUsed))}</span></div>
      </div>
      ${!sched && others ? `<div class="text-3 mt-sm">${escapeHtml(T('snapshots.other_schedules', { n: others }))}</div>` : ''}`;
    el.querySelector('[data-act="edit-schedule"]')?.addEventListener('click', () => openSnapshotScheduleEditor(screen, { schedule: sched, datasets, dataset: name, onDone: reloadSchedules }));
    el.querySelector('[data-act="delete-schedule"]')?.addEventListener('click', async () => {
      const ok = await TfWindow.confirm({ title: T('snapshots.schedule_delete'), message: T('snapshots.schedule_delete_confirm', { dataset: sched.dataset }), confirmLabel: I18n.t('common.delete'), cancelLabel: I18n.t('common.cancel'), danger: true });
      if (!ok) return;
      try {
        await screen.nas('tentaNasSnapshotScheduleDeleteRequest', { scheduleId: sched.scheduleId });
        toast(T('snapshots.schedule_deleted'), 'success');
        reloadSchedules();
      } catch (e) {
        toast(errMessage(e), 'error');
      }
    });
    paintSmbCard(screen, host.querySelector('#nas-snap-smb'), focusDataset(), state.shares, reloadSchedules);
  };

  const reloadList = async () => {
    if (screen.disposed || !host.isConnected) return;
    const origin = state.filter === 'auto' || state.filter === 'manual' ? state.filter : '';
    try {
      const r = await screen.nas('tentaNasSnapshotsListRequest', { pool, dataset: state.dataset, recursive: true, origin, limit: LIST_LIMIT });
      state.snapshots = r.snapshots || [];
      state.total = Number(r.total) || state.snapshots.length;
      state.totalUsed = Number(r.totalUsedBytes) || 0;
    } catch (e) {
      toast(errMessage(e), 'error');
      return;
    }
    if (screen.disposed || !host.isConnected) return;
    applyRows();
  };

  const applyRows = () => {
    const dayAgo = Date.now() - 86400000;
    const rows = state.snapshots.filter((s) => {
      if (state.query && !s.name.toLowerCase().includes(state.query)) return false;
      if (state.filter === 'day') { const t = parseServerTs(s.createdAt); return t && t.getTime() >= dayAgo; }
      return true;
    });
    host.querySelector('#nas-snap-total').setAttribute('label', String(state.total));
    filters.filters = ['all', 'auto', 'manual', 'day'].map((id) => ({ id, label: T('snapshots.filter_' + id), count: id === 'all' ? state.total : undefined, active: id === state.filter }));
    table.rows = rows.map((s) => snapshotRow(s, Boolean(state.dataset)));
  };

  table.rowActions = (row) => {
    const s = row._snap;
    const wrap = document.createElement('div');
    wrap.className = 'tf-table__cell-row';
    wrap.innerHTML = `
      <tf-button size="sm" variant="secondary" data-act="browse">${escapeHtml(T('snapshots.browse'))}</tf-button>
      ${admin ? `
      <tf-button size="sm" variant="secondary" data-act="clone">${escapeHtml(T('snapshots.clone'))}</tf-button>
      <tf-button size="sm" variant="secondary" data-act="rollback">${escapeHtml(T('snapshots.rollback'))}</tf-button>
      <tf-button size="sm" variant="ghost" tone="critical" icon="trash" data-act="delete" title="${escapeAttr(I18n.t('common.delete'))}"></tf-button>` : ''}`;
    wrap.querySelector('[data-act="browse"]').addEventListener('click', (e) => { e.stopPropagation(); openSnapshotBrowser(screen, { snapshot: s }); });
    wrap.querySelector('[data-act="rollback"]')?.addEventListener('click', (e) => { e.stopPropagation(); openRollbackDialog(screen, { snapshot: s, newer: newerThan(state.snapshots, s), pool, onDone: reloadAll }); });
    wrap.querySelector('[data-act="clone"]')?.addEventListener('click', (e) => { e.stopPropagation(); openCloneDialog(screen, { snapshot: s, pool, onDone: reloadAll }); });
    wrap.querySelector('[data-act="delete"]')?.addEventListener('click', (e) => { e.stopPropagation(); destroySnapshots(screen, [s.name], reloadAll); });
    return wrap;
  };

  const reloadAll = async () => { await reloadList(); await reloadSchedules(); if (onChange) onChange(); };
  host.querySelector('[data-act="snap-now"]')?.addEventListener('click', () => openSnapshotNowDialog(screen, { dataset: state.dataset || pool, datasets, onDone: reloadAll }));

  await reloadList();
  await reloadSchedules();
}

// The SMB "Previous Versions" switch belongs to the share that exports the
// dataset: flipping it resends the share's options with `previousVersions`
// changed. Without an SMB share the switch stays off and disabled.
function paintSmbCard(screen, el, dataset, shares, onDone) {
  const share = shares.find((s) => s.protocol === 'smb' && (s.dataset === dataset.name || (dataset.mountpoint && s.sourcePath === dataset.mountpoint))) || null;
  const on = Boolean(share?.smb?.previousVersions);
  el.innerHTML = `
    <div class="section-card-head"><div class="title">${escapeHtml(T('snapshots.smb_title'))}</div></div>
    <div class="toggle-card">
      <div class="tc-text"><span>${escapeHtml(T('snapshots.smb_previous'))}</span><span class="tc-sub">${escapeHtml(share ? T('snapshots.smb_previous_sub') : T('snapshots.smb_no_share', { dataset: dataset.name }))}</span></div>
      <tf-toggle id="nas-snap-prev" ${on ? 'checked' : ''} ${share && screen.isAdmin ? '' : 'disabled'}></tf-toggle>
    </div>
    <div class="explain-box mt-sm">${T('snapshots.smb_explain')}</div>`;
  const toggle = el.querySelector('#nas-snap-prev');
  toggle.addEventListener('change', async () => {
    if (!share) return;
    const previousVersions = Boolean(toggle.checked);
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasShareUpdateRequest', {
      shareId: share.shareId,
      smb: { ...share.smb, previousVersions },
      nfs: share.nfs || null,
      fleetMount: Boolean(share.fleetMount),
      enabled: Boolean(share.enabled),
      sudoPassword,
    }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('wizard_share.sudo_title_edit', { name: share.name }));
    if (res === null) { toggle.checked = on; return; }
    followResponse(screen, res, onDone, T('snapshots.smb_saved', { name: share.name }));
  });
}

function snapshotRow(s, singleDataset) {
  const manual = s.origin !== 'auto';
  return {
    _snap: s,
    name: `<div class="tf-table__cell-row"><span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(s.shortName)}</span></span>${manual ? ` <tf-chip size="sm" status="accent" label="${escapeAttr(T('snapshots.origin_manual'))}"></tf-chip>` : ''}</div>${singleDataset ? '' : `<div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(s.dataset)}</div>`}`,
    created: `<span>${escapeHtml(fmtAgo(s.createdAt))}</span><div class="tf-table__cell-sub">${escapeHtml(fmtDate(s.createdAt))}</div>`,
    used: `<span class="tf-table__cell--mono">${escapeHtml(fmtBytes(s.usedBytes))}</span>`,
    origin: `<tf-chip size="sm" status="${manual ? 'accent' : 'neutral'}" label="${escapeAttr(manual ? T('snapshots.origin_manual') : (s.tier ? T('snapshots.origin_auto_tier', { tier: tierLabel(s.tier) }) : T('snapshots.origin_auto')))}"></tf-chip>`,
  };
}

const TIER_KEYS = { frequent: 'tier_frequent', hourly: 'tier_hourly', daily: 'tier_daily', weekly: 'tier_weekly', monthly: 'tier_monthly' };
const tierLabel = (tier) => (TIER_KEYS[tier] ? T('snapshots.' + TIER_KEYS[tier]) : tier);

// ---------------------------------------------------------------------------
// Read-only snapshot browser
// ---------------------------------------------------------------------------

// Walks the directories of one snapshot through `SnapshotBrowseRequest`;
// nothing here writes. The breadcrumb starts at the snapshot itself.
export function openSnapshotBrowser(screen, { snapshot }) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('snapshots.browse_title', { name: snapshot.shortName }));
  win.setAttribute('subtitle', snapshot.dataset);
  win.setAttribute('icon', 'folder');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '640');
  win.setAttribute('min-width', '480');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="explain-box">${escapeHtml(T('snapshots.browse_explain'))}</div>
      <div id="nas-sbr-crumbs"></div>
      <tf-table id="nas-sbr-table" empty-message="${escapeAttr(T('snapshots.browse_empty'))}">
        <tf-column key="name" label="${escapeAttr(T('wizard_share.browse_col_name'))}" renderer="html" fill></tf-column>
      </tf-table>
      <div class="text-3 mono" id="nas-sbr-path"></div>
      <div class="num-err" id="nas-sbr-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.close'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const table = win.querySelector('#nas-sbr-table');
  const state = { path: '', entries: [] };

  const go = async (p) => {
    const err = win.querySelector('#nas-sbr-error');
    err.hidden = true;
    try {
      const r = await screen.nas('tentaNasSnapshotBrowseRequest', { snapshot: snapshot.name, path: p });
      state.path = r.path || '';
      state.entries = r.entries || [];
    } catch (e) {
      err.textContent = errMessage(e);
      err.hidden = false;
      return;
    }
    if (!win.isConnected) return;
    const crumbEl = win.querySelector('#nas-sbr-crumbs');
    crumbEl.innerHTML = pathCrumbsHtml(snapshot.shortName, state.path);
    wirePathCrumbs(crumbEl, state.path, go);
    win.querySelector('#nas-sbr-path').textContent = `${snapshot.dataset}/.zfs/snapshot/${snapshot.shortName}${state.path ? '/' + state.path : ''}`;
    table.rows = state.entries.map((e) => ({
      _entry: e,
      name: `<div class="tf-table__cell-row">${sprite('folder')}<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(e.name)}</span></span></div>`,
    }));
  };
  table.addEventListener('row-click', (e) => go(e.detail.row._entry.path));
  win.addEventListener('action', (e) => { if (e.detail?.action === 'cancel') win.close(true); });
  go('');
  return win;
}

// Snapshots of the same dataset created after `s` — the ones a rollback to
// `s` would have to destroy.
export function newerThan(all, s) {
  const t = parseServerTs(s.createdAt)?.getTime() ?? 0;
  return all
    .filter((x) => x.dataset === s.dataset && x.name !== s.name && (parseServerTs(x.createdAt)?.getTime() ?? 0) > t)
    .sort((a, b) => (parseServerTs(a.createdAt)?.getTime() ?? 0) - (parseServerTs(b.createdAt)?.getTime() ?? 0));
}

// "96 × 15 min · 30 dzienne · 12 miesięczne" — the frequent tier is
// counted in units of the cadence, the calendar tiers by name.
export function keepSummary(s) {
  const parts = [];
  if (s.keepFrequent) parts.push(T('snapshots.keep_frequent_n', { n: s.keepFrequent, every: fmtSchedule(s.schedule) }));
  if (s.keepHourly) parts.push(T('snapshots.keep_hourly_n', { n: s.keepHourly }));
  if (s.keepDaily) parts.push(T('snapshots.keep_daily_n', { n: s.keepDaily }));
  if (s.keepWeekly) parts.push(T('snapshots.keep_weekly_n', { n: s.keepWeekly }));
  if (s.keepMonthly) parts.push(T('snapshots.keep_monthly_n', { n: s.keepMonthly }));
  return parts.length ? parts.join(' · ') : T('snapshots.keep_none');
}

async function destroySnapshots(screen, names, onDone) {
  if (!names.length) return;
  const ok = await TfWindow.confirm({
    title: T('snapshots.delete_title', { n: names.length }),
    message: T('snapshots.delete_confirm', { n: names.length, first: names[0] }),
    confirmLabel: I18n.t('common.delete'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
  if (!ok) return;
  const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasSnapshotDestroyRequest', { names, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('snapshots.delete_title', { n: names.length }));
  followResponse(screen, res, onDone, T('snapshots.deleted_done', { n: names.length }));
}

// ---------------------------------------------------------------------------
// Snapshot now / clone
// ---------------------------------------------------------------------------

export function openSnapshotNowDialog(screen, { dataset, datasets = null, onDone }) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('snapshots.now'));
  win.setAttribute('subtitle', dataset);
  win.setAttribute('icon', 'save');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const stamp = new Date().toISOString().slice(0, 16).replace('T', '-').replace(':', '');
  win.innerHTML = `
    <div slot="body" class="stack">
      ${datasets ? `<tf-select id="nas-sn-dataset" label="${escapeAttr(T('snapshots.dataset'))}"></tf-select>` : ''}
      <tf-input id="nas-sn-name" label="${escapeAttr(T('snapshots.name_label'))}" value="manual-${escapeAttr(stamp)}" autocomplete="off" spellcheck="false" hint="${escapeAttr(T('snapshots.name_hint'))}"></tf-input>
      <tf-checkbox id="nas-sn-recursive" label="${escapeAttr(T('snapshots.recursive_label'))}"></tf-checkbox>
      <div class="num-err" id="nas-sn-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="save" data-action="confirm">${escapeHtml(T('snapshots.create'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const dsSel = win.querySelector('#nas-sn-dataset');
  if (dsSel) dsSel.setOptions(datasets.map((d) => ({ value: d.name, label: d.name })), dataset);
  let busy = false;
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    const shortName = win.querySelector('#nas-sn-name').value.trim();
    const target = dsSel ? dsSel.value : dataset;
    if (!/^[a-zA-Z0-9_.:-]+$/.test(shortName)) {
      win.querySelector('#nas-sn-name').setAttribute('error', T('snapshots.name_invalid'));
      return;
    }
    busy = true;
    try {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasSnapshotCreateRequest', { dataset: target, shortName, recursive: Boolean(win.querySelector('#nas-sn-recursive').checked), sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('snapshots.now'));
      busy = false;
      if (res === null) return;
      win.close(true);
      followResponse(screen, res, onDone, T('snapshots.created_done', { name: `${target}@${shortName}` }));
    } catch (err) {
      busy = false;
      const errEl = win.querySelector('#nas-sn-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  return win;
}

export function openCloneDialog(screen, { snapshot, pool, onDone }) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('snapshots.clone'));
  win.setAttribute('subtitle', snapshot.name);
  win.setAttribute('icon', 'copy');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="explain-box">${escapeHtml(T('snapshots.clone_explain'))}</div>
      <tf-input id="nas-cl-target" label="${escapeAttr(T('snapshots.clone_target'))}" prefix="${escapeAttr(pool + '/')}" value="${escapeAttr(snapshot.dataset.split('/').slice(1).join('/') + '-clone')}" autocomplete="off" spellcheck="false"></tf-input>
      <div class="num-err" id="nas-cl-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="copy" data-action="confirm">${escapeHtml(T('snapshots.clone'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  let busy = false;
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    const short = win.querySelector('#nas-cl-target').value.trim().replace(/^\/+/, '');
    if (!short) return;
    const target = `${pool}/${short}`;
    busy = true;
    try {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasSnapshotCloneRequest', { name: snapshot.name, target, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('snapshots.clone'));
      busy = false;
      if (res === null) return;
      win.close(true);
      followResponse(screen, res, onDone, T('snapshots.cloned_done', { target }));
    } catch (err) {
      busy = false;
      const errEl = win.querySelector('#nas-cl-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  return win;
}

// ---------------------------------------------------------------------------
// Rollback (n17b)
// ---------------------------------------------------------------------------

export function openRollbackDialog(screen, { snapshot, newer = [], pool = snapshot.dataset.split('/')[0], onDone }) {
  const destroyNewer = newer.length > 0;
  const bodyHtml = `
    <div class="wizard-warning danger">${sprite('alert')}<div>${T('rollback.warning', { dataset: `<b class="mono">${escapeHtml(snapshot.dataset)}</b>`, ago: `<b>${escapeHtml(fmtAgoSpan(snapshot.createdAt))}</b>` })}
      ${destroyNewer ? `${escapeHtml(T('rollback.newer_lost', { n: newer.length }))}<div class="snap-lost">${newer.map((s) => `<span class="mono">${escapeHtml(s.shortName)}</span>`).join(' · ')}</div>` : escapeHtml(T('rollback.newer_none'))}
    </div></div>
    <div class="explain-box mt-md">${T('rollback.clone_hint')}</div>`;
  return openRetypeDialog({
    title: T('rollback.title', { name: snapshot.shortName }),
    icon: 'rotate',
    name: snapshot.shortName,
    bodyHtml,
    retypeLabel: escapeHtml(T('rollback.retype')),
    confirmLabel: destroyNewer ? T('rollback.confirm_destroy', { n: newer.length }) : T('rollback.confirm'),
    confirmIcon: 'rotate',
    width: 560,
    secondary: { label: T('rollback.clone_instead'), icon: 'copy', onClick: () => openCloneDialog(screen, { snapshot, pool, onDone }) },
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasSnapshotRollbackRequest', { name: snapshot.name, confirmName: snapshot.name, destroyNewer, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('rollback.confirm'));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('rollback.done', { name: snapshot.shortName }));
      return true;
    },
  });
}

// "51 minut" — the age of the snapshot as a bare span (no "ago"), in the
// coarsest unit that still reads naturally.
const fmtAgoSpan = (ts) => {
  const t = parseServerTs(ts);
  if (!t) return '—';
  const secs = Math.max(0, Math.round((Date.now() - t.getTime()) / 1000));
  if (secs < 3600) return T('rollback.span_minutes', { n: Math.max(1, Math.round(secs / 60)) });
  if (secs < 86400) return T('rollback.span_hours', { n: Math.round(secs / 3600) });
  return T('rollback.span_days', { n: Math.round(secs / 86400) });
};

// ---------------------------------------------------------------------------
// Snapshot schedule editor (cadence + GFS retention)
// ---------------------------------------------------------------------------

export function openSnapshotScheduleEditor(screen, { schedule = null, datasets = [], dataset = '', onDone }) {
  const editing = Boolean(schedule && schedule.scheduleId);
  const initial = {
    scheduleId: editing ? schedule.scheduleId : '',
    dataset: editing ? schedule.dataset : (dataset || datasets[0]?.name || ''),
    enabled: editing ? Boolean(schedule.enabled) : true,
    recursive: editing ? Boolean(schedule.recursive) : true,
    schedule: normalizeSchedule(editing ? schedule.schedule : { every: '1h' }),
    ...DEFAULT_KEEP,
  };
  if (editing) for (const k of KEEP_KEYS) initial[k] = Number(schedule[k]) || 0;

  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', editing ? T('snapshots.schedule_edit') : T('snapshots.schedule_add'));
  win.setAttribute('icon', 'clock');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '620');
  win.setAttribute('min-width', '480');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <tf-select id="nas-ss-dataset" label="${escapeAttr(T('snapshots.dataset'))}" ${editing ? 'disabled' : ''}></tf-select>
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('schedule.enabled'))}</span><span class="tc-sub">${escapeHtml(T('snapshots.enabled_sub'))}</span></div>
        <tf-toggle id="nas-ss-enabled" ${initial.enabled ? 'checked' : ''}></tf-toggle>
      </div>
      <tf-checkbox id="nas-ss-recursive" label="${escapeAttr(T('snapshots.recursive_label'))}" ${initial.recursive ? 'checked' : ''}></tf-checkbox>
      ${scheduleFieldsHtml('nas-ss', initial.schedule)}
      <div class="wizard-section-title">${escapeHtml(T('snapshots.keep_title'))}</div>
      <div class="wizard-section-sub">${escapeHtml(T('snapshots.keep_sub'))}</div>
      <div class="keep-grid">
        ${KEEP_KEYS.map((k) => `<tf-input id="nas-ss-${k}" type="number" min="0" max="999" step="1" inputmode="numeric" label="${escapeAttr(T('snapshots.' + k))}" value="${initial[k]}"></tf-input>`).join('')}
      </div>
      <div class="muted" id="nas-ss-preview"></div>
      <div class="num-err" id="nas-ss-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="save" data-action="confirm">${escapeHtml(T('schedule.save'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const dsSel = win.querySelector('#nas-ss-dataset');
  const dsOptions = datasets.map((d) => ({ value: d.name, label: d.name }));
  if (initial.dataset && !dsOptions.some((o) => o.value === initial.dataset)) dsOptions.unshift({ value: initial.dataset, label: initial.dataset });
  dsSel.setOptions(dsOptions, initial.dataset);
  wireScheduleFields(win, 'nas-ss', initial.schedule);

  const read = () => {
    const out = {
      scheduleId: initial.scheduleId,
      dataset: dsSel.value,
      enabled: Boolean(win.querySelector('#nas-ss-enabled').checked),
      recursive: Boolean(win.querySelector('#nas-ss-recursive').checked),
      schedule: readScheduleFields(win, 'nas-ss'),
    };
    for (const k of KEEP_KEYS) out[k] = Math.max(0, Math.round(Number(win.querySelector('#nas-ss-' + k).value) || 0));
    return out;
  };
  const preview = () => { win.querySelector('#nas-ss-preview').textContent = keepSummary(read()); };
  for (const k of KEEP_KEYS) win.querySelector('#nas-ss-' + k).addEventListener('input', preview);
  win.addEventListener('change', preview);
  preview();

  let busy = false;
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    const payload = read();
    if (!payload.dataset) return;
    busy = true;
    try {
      const res = await screen.nas('tentaNasSnapshotScheduleSetRequest', payload);
      toast(T('schedule.saved'), 'success');
      win.close(true);
      if (onDone) onDone(res);
    } catch (err) {
      busy = false;
      const errEl = win.querySelector('#nas-ss-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  return win;
}
