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
  fmtDate, fmtAgo, fmtIn, fmtBytes, errMessage, fmtSchedule,
} from '/js/modules/tentanas/format.js';
import { scheduleFieldsHtml, wireScheduleFields, readScheduleFields, normalizeSchedule } from '/js/modules/tentanas/schedule-editor.js';
import { openRetypeDialog, followResponse, warningHtml } from '/js/modules/tentanas/dialogs.js';
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
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('clock')} ${escapeHtml(T('snapshots.schedules'))}</div>
          <div class="actions">${admin ? `<tf-button variant="secondary" size="sm" icon="plus" data-act="add-schedule">${escapeHtml(T('snapshots.schedule_add'))}</tf-button>` : ''}</div>
        </div>
        <div id="nas-snap-schedules"></div>
      </div>
      <div class="section-card">
        <div class="tf-toolbar">
          <tf-select id="nas-snap-dataset"></tf-select>
          <tf-searchbox id="nas-snap-search" placeholder="${escapeAttr(T('snapshots.search'))}" debounce="150"></tf-searchbox>
          <tf-filter-chips id="nas-snap-filters"></tf-filter-chips>
          <span class="tf-toolbar-spacer"></span>
          <span class="muted" id="nas-snap-hint"></span>
          ${admin ? `
          <tf-button variant="danger" size="sm" icon="trash" data-act="bulk-delete" disabled>${escapeHtml(T('snapshots.delete_selected', { n: 0 }))}</tf-button>
          <tf-button variant="primary" size="sm" icon="save" data-act="snap-now">${escapeHtml(T('snapshots.now'))}</tf-button>` : ''}
        </div>
        <tf-table id="nas-snap-table" ${admin ? 'selectable="multi"' : ''} empty-message="${escapeAttr(T('snapshots.none'))}">
          <tf-column key="name" label="${escapeAttr(T('snapshots.col_name'))}" renderer="html" fill sortable></tf-column>
          <tf-column key="created" label="${escapeAttr(T('snapshots.col_created'))}" renderer="html" nowrap sortable></tf-column>
          <tf-column key="used" label="${escapeAttr(T('snapshots.col_used'))}" renderer="text" nowrap hide-below="900"></tf-column>
          <tf-column key="referenced" label="${escapeAttr(T('snapshots.col_referenced'))}" renderer="text" nowrap hide-below="1100"></tf-column>
          <tf-column key="origin" label="${escapeAttr(T('snapshots.col_type'))}" renderer="html" nowrap></tf-column>
          <tf-column key="extra" label="${escapeAttr(T('snapshots.col_extra'))}" renderer="html" hide-below="1200"></tf-column>
        </tf-table>
      </div>
    </div>`;

  const state = { pool, dataset: '', query: '', filter: 'all', snapshots: [], schedules: [], selected: new Set(), total: 0 };
  const table = host.querySelector('#nas-snap-table');
  const dsSel = host.querySelector('#nas-snap-dataset');
  dsSel.setOptions([{ value: '', label: T('snapshots.all_datasets', { pool }) }, ...datasets.map((d) => ({ value: d.name, label: d.name }))], '');
  dsSel.addEventListener('change', (e) => { state.dataset = e.detail.value; reloadList(); });
  const filters = host.querySelector('#nas-snap-filters');
  filters.filters = ['all', 'auto', 'manual', 'day'].map((id) => ({ id, label: T('snapshots.filter_' + id), active: id === state.filter }));
  filters.addEventListener('change', (e) => { state.filter = e.detail.id; reloadList(); });
  host.querySelector('#nas-snap-search').addEventListener('search', (e) => { state.query = (e.detail.value || '').trim().toLowerCase(); applyRows(); });

  const reloadSchedules = async () => {
    try {
      const r = await screen.nas('tentaNasSnapshotSchedulesListRequest', {});
      state.schedules = (r.schedules || []).filter((s) => s.dataset === pool || s.dataset.startsWith(pool + '/'));
    } catch (e) {
      toast(errMessage(e), 'error');
      return;
    }
    if (!host.isConnected) return;
    paintSchedules();
  };

  const paintSchedules = () => {
    const el = host.querySelector('#nas-snap-schedules');
    if (!state.schedules.length) {
      el.innerHTML = `<div class="muted">${escapeHtml(T('snapshots.schedules_none'))}</div>`;
      return;
    }
    el.innerHTML = `<div class="sched-rows">${state.schedules.map((s) => `
      <div class="sched-row" data-id="${escapeAttr(s.scheduleId)}">
        <div class="sched-main">
          <div class="row"><span class="mono">${escapeHtml(s.dataset)}</span>${s.recursive ? `<tf-chip size="sm" status="info" label="${escapeAttr(T('snapshots.recursive'))}"></tf-chip>` : ''}<tf-chip size="sm" status="${s.enabled ? 'ok' : 'warn'}" dot label="${escapeAttr(s.enabled ? T('schedule.on') : T('schedule.off'))}"></tf-chip></div>
          <div class="text-3">${escapeHtml(keepSummary(s))} · ${escapeHtml(T('snapshots.schedule_count', { n: s.snapshotCount }))}${s.nextRunAt ? ` · ${escapeHtml(T('snapshots.next_run', { t: fmtIn(s.nextRunAt) }))}` : ''}</div>
        </div>
        <span class="sched-pill">${sprite('clock')} ${escapeHtml(fmtSchedule(s.schedule))}</span>
        ${admin ? `<div class="row">
          <tf-button size="sm" variant="ghost" icon="edit" data-act="edit"></tf-button>
          <tf-button size="sm" variant="ghost" tone="critical" icon="trash" data-act="delete"></tf-button>
        </div>` : ''}
      </div>`).join('')}</div>`;
    el.querySelectorAll('.sched-row').forEach((row) => {
      const s = state.schedules.find((x) => x.scheduleId === row.dataset.id);
      row.querySelector('[data-act="edit"]')?.addEventListener('click', () => openSnapshotScheduleEditor(screen, { schedule: s, datasets, onDone: reloadSchedules }));
      row.querySelector('[data-act="delete"]')?.addEventListener('click', async () => {
        const ok = await TfWindow.confirm({ title: T('snapshots.schedule_delete'), message: T('snapshots.schedule_delete_confirm', { dataset: s.dataset }), confirmLabel: I18n.t('common.delete'), cancelLabel: I18n.t('common.cancel'), danger: true });
        if (!ok) return;
        try {
          await screen.nas('tentaNasSnapshotScheduleDeleteRequest', { scheduleId: s.scheduleId });
          toast(T('snapshots.schedule_deleted'), 'success');
          reloadSchedules();
        } catch (e) {
          toast(errMessage(e), 'error');
        }
      });
    });
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
    state.selected.clear();
    applyRows();
  };

  const applyRows = () => {
    const dayAgo = Date.now() - 86400000;
    const rows = state.snapshots.filter((s) => {
      if (state.query && !s.name.toLowerCase().includes(state.query)) return false;
      if (state.filter === 'day') { const t = parseServerTs(s.createdAt); return t && t.getTime() >= dayAgo; }
      return true;
    });
    host.querySelector('#nas-snap-hint').textContent = T('snapshots.hint', { n: rows.length, total: state.total, size: fmtBytes(state.totalUsed) });
    table.rows = rows.map((s) => snapshotRow(s, state.selected.has(s.name)));
    syncBulk();
  };

  const syncBulk = () => {
    const btn = host.querySelector('[data-act="bulk-delete"]');
    if (!btn) return;
    const n = state.selected.size;
    btn.textContent = T('snapshots.delete_selected', { n });
    if (n) btn.removeAttribute('disabled'); else btn.setAttribute('disabled', '');
  };

  table.addEventListener('row-select', (e) => {
    const name = e.detail.row._snap.name;
    if (e.detail.selected) state.selected.add(name); else state.selected.delete(name);
    syncBulk();
  });
  table.addEventListener('select-all', (e) => {
    state.selected.clear();
    if (e.detail.selected) for (const row of table.rows) state.selected.add(row._snap.name);
    syncBulk();
  });
  table.rowActions = (row) => {
    if (!admin) return null;
    const s = row._snap;
    const wrap = document.createElement('div');
    wrap.className = 'tf-table__cell-row';
    wrap.innerHTML = `
      <tf-button size="sm" variant="ghost" icon="rotate" data-act="rollback" title="${escapeAttr(T('snapshots.rollback'))}"></tf-button>
      <tf-button size="sm" variant="ghost" icon="copy" data-act="clone" title="${escapeAttr(T('snapshots.clone'))}"></tf-button>
      <tf-button size="sm" variant="ghost" tone="critical" icon="trash" data-act="delete" title="${escapeAttr(I18n.t('common.delete'))}"></tf-button>`;
    wrap.querySelector('[data-act="rollback"]').addEventListener('click', (e) => { e.stopPropagation(); openRollbackDialog(screen, { snapshot: s, newer: newerThan(state.snapshots, s), onDone: reloadAll }); });
    wrap.querySelector('[data-act="clone"]').addEventListener('click', (e) => { e.stopPropagation(); openCloneDialog(screen, { snapshot: s, pool, onDone: reloadAll }); });
    wrap.querySelector('[data-act="delete"]').addEventListener('click', (e) => { e.stopPropagation(); destroySnapshots(screen, [s.name], reloadAll); });
    return wrap;
  };

  const reloadAll = () => { reloadList(); reloadSchedules(); if (onChange) onChange(); };
  host.querySelector('[data-act="bulk-delete"]')?.addEventListener('click', () => destroySnapshots(screen, [...state.selected], reloadAll));
  host.querySelector('[data-act="snap-now"]')?.addEventListener('click', () => openSnapshotNowDialog(screen, { dataset: state.dataset || pool, datasets, onDone: reloadAll }));
  host.querySelector('[data-act="add-schedule"]')?.addEventListener('click', () => openSnapshotScheduleEditor(screen, { schedule: null, datasets, dataset: state.dataset || pool, onDone: reloadSchedules }));

  await Promise.all([reloadList(), reloadSchedules()]);
}

function snapshotRow(s, selected) {
  return {
    _snap: s,
    _selected: selected,
    name: `<span class="tf-table__cell-title tf-table__cell--mono">${escapeHtml(s.shortName)}</span><div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(s.dataset)}</div>`,
    created: `<span>${escapeHtml(fmtAgo(s.createdAt))}</span><div class="tf-table__cell-sub">${escapeHtml(fmtDate(s.createdAt))}</div>`,
    used: fmtBytes(s.usedBytes),
    referenced: fmtBytes(s.referencedBytes),
    origin: `<tf-chip size="sm" status="${s.origin === 'auto' ? 'info' : 'accent'}" label="${escapeAttr(s.origin === 'auto' ? T('snapshots.origin_auto') : T('snapshots.origin_manual'))}"></tf-chip>${s.tier ? ` <tf-chip size="sm" label="${escapeAttr(s.tier)}"></tf-chip>` : ''}`,
    extra: [
      (s.clones || []).length ? T('snapshots.clones', { n: s.clones.length }) : '',
      Number(s.holds) ? T('snapshots.holds', { n: s.holds }) : '',
    ].filter(Boolean).map((t) => `<span class="tf-table__cell-sub">${escapeHtml(t)}</span>`).join(' ') || '<span class="tf-table__cell-sub">—</span>',
  };
}

// Snapshots of the same dataset created after `s` — the ones a rollback to
// `s` would have to destroy.
export function newerThan(all, s) {
  const t = parseServerTs(s.createdAt)?.getTime() ?? 0;
  return all
    .filter((x) => x.dataset === s.dataset && x.name !== s.name && (parseServerTs(x.createdAt)?.getTime() ?? 0) > t)
    .sort((a, b) => (parseServerTs(a.createdAt)?.getTime() ?? 0) - (parseServerTs(b.createdAt)?.getTime() ?? 0));
}

function keepSummary(s) {
  const parts = [];
  if (s.keepFrequent) parts.push(T('snapshots.keep_frequent_n', { n: s.keepFrequent }));
  if (s.keepHourly) parts.push(T('snapshots.keep_hourly_n', { n: s.keepHourly }));
  if (s.keepDaily) parts.push(T('snapshots.keep_daily_n', { n: s.keepDaily }));
  if (s.keepWeekly) parts.push(T('snapshots.keep_weekly_n', { n: s.keepWeekly }));
  if (s.keepMonthly) parts.push(T('snapshots.keep_monthly_n', { n: s.keepMonthly }));
  return parts.length ? T('snapshots.keep_prefix', { list: parts.join(', ') }) : T('snapshots.keep_none');
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

export function openRollbackDialog(screen, { snapshot, newer = [], onDone }) {
  const destroyNewer = newer.length > 0;
  const bodyHtml = `
    ${warningHtml('danger', T('rollback.warning', { dataset: snapshot.dataset, at: fmtDate(snapshot.createdAt) }))}
    <div class="explain-box">${escapeHtml(T('rollback.explain', { name: snapshot.shortName, at: fmtAgo(snapshot.createdAt) }))}</div>
    ${destroyNewer ? `
      <div class="snap-lost">
        <div>${escapeHtml(T('rollback.newer_lost', { n: newer.length }))}</div>
        ${newer.map((s) => `<div class="mono">${escapeHtml(s.shortName)} <span class="text-3">· ${escapeHtml(fmtAgo(s.createdAt))} · ${escapeHtml(fmtBytes(s.usedBytes))}</span></div>`).join('')}
      </div>` : `<div class="muted">${escapeHtml(T('rollback.newer_none'))}</div>`}`;
  return openRetypeDialog({
    title: T('rollback.title'),
    subtitle: snapshot.name,
    icon: 'rotate',
    name: snapshot.name,
    bodyHtml,
    confirmLabel: destroyNewer ? T('rollback.confirm_destroy', { n: newer.length }) : T('rollback.confirm'),
    confirmIcon: 'rotate',
    width: 600,
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasSnapshotRollbackRequest', { name: snapshot.name, confirmName: snapshot.name, destroyNewer, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('rollback.title'));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('rollback.done', { name: snapshot.shortName }));
      return true;
    },
  });
}

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
