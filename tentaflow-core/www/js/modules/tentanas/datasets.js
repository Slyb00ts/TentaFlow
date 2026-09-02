// ===== File: modules/tentanas/datasets.js — the Datasets inner tab of a pool (n09): dataset tree, per-dataset properties, create/destroy, key and mount actions =====
//
// A pool's datasets form a tree by name (`tank/media/movies`); the table
// shows it indented in one flat list so sorting and filtering stay simple.
// Selecting a row loads the full property set for that dataset below.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import {
  T, sprite, ADMIN_TIMEOUT_MS,
  fmtAgo, fmtBytes, fmtRatio, pct, errMessage, fmtSchedule,
} from '/js/modules/tentanas/format.js';
import { openRetypeDialog, followResponse, dangerRowHtml, warningHtml } from '/js/modules/tentanas/dialogs.js';
import { openPropertyEditor, sourceChipHtml } from '/js/modules/tentanas/pool-detail.js';
import { openSnapshotNowDialog } from '/js/modules/tentanas/snapshots.js';
import '/js/components/tf-table.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-progress-bar.js';
import '/js/components/tf-window.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-checkbox.js';

const GIB = 1024 ** 3;
const TIB = 1024 ** 4;
const COMPRESSION = ['inherit', 'zstd', 'lz4', 'gzip', 'off'];
const RECORDSIZE = ['inherit', '16K', '64K', '128K', '1M'];
const VOLBLOCK = ['16K', '8K', '32K', '64K', '128K'];

export async function drawDatasets(screen, host, { pool, onChange = null }) {
  host.innerHTML = `
    <div class="stack">
      <div class="section-card">
        <div class="tf-toolbar">
          <tf-searchbox id="nas-ds-search" placeholder="${escapeAttr(T('datasets.search'))}" debounce="150"></tf-searchbox>
          <tf-filter-chips id="nas-ds-filters"></tf-filter-chips>
          <span class="tf-toolbar-spacer"></span>
          <span class="muted" id="nas-ds-hint"></span>
          ${screen.isAdmin ? `
          <tf-button variant="secondary" size="sm" icon="database" data-act="create-volume">${escapeHtml(T('datasets.new_volume'))}</tf-button>
          <tf-button variant="primary" size="sm" icon="plus" data-act="create">${escapeHtml(T('datasets.new'))}</tf-button>` : ''}
        </div>
        <tf-table id="nas-ds-table" empty-message="${escapeAttr(T('datasets.none'))}">
          <tf-column key="name" label="${escapeAttr(T('datasets.col_name'))}" renderer="html" fill></tf-column>
          <tf-column key="compression" label="${escapeAttr(T('datasets.col_compression'))}" renderer="html" nowrap hide-below="900"></tf-column>
          <tf-column key="quota" label="${escapeAttr(T('datasets.col_quota'))}" renderer="text" nowrap hide-below="1000"></tf-column>
          <tf-column key="usage" label="${escapeAttr(T('datasets.col_usage'))}" renderer="html" width="200"></tf-column>
          <tf-column key="snapshots" label="${escapeAttr(T('datasets.col_snapshots'))}" renderer="html" nowrap hide-below="1100"></tf-column>
          <tf-column key="mount" label="${escapeAttr(T('datasets.col_mount'))}" renderer="html" hide-below="1200"></tf-column>
        </tf-table>
      </div>
      <div id="nas-ds-detail"></div>
    </div>`;

  const state = { pool, datasets: [], query: '', filter: 'all', selected: screen.dataset || null };
  const table = host.querySelector('#nas-ds-table');
  const filters = host.querySelector('#nas-ds-filters');
  filters.filters = ['all', 'filesystem', 'volume', 'encrypted', 'scheduled'].map((id) => ({ id, label: T('datasets.filter_' + id), active: id === state.filter }));
  filters.addEventListener('change', (e) => { state.filter = e.detail.id; applyRows(); });
  host.querySelector('#nas-ds-search').addEventListener('search', (e) => { state.query = (e.detail.value || '').trim().toLowerCase(); applyRows(); });

  const reload = async () => {
    if (screen.disposed || !host.isConnected) return;
    try {
      const r = await screen.nas('tentaNasDatasetsListRequest', { pool });
      state.datasets = (r.datasets || []).slice().sort((a, b) => a.name.localeCompare(b.name));
    } catch (e) {
      toast(errMessage(e), 'error');
      return;
    }
    if (screen.disposed || !host.isConnected) return;
    applyRows();
    if (state.selected && !state.datasets.some((d) => d.name === state.selected)) state.selected = null;
    drawDetail();
    if (onChange) onChange();
  };

  const applyRows = () => {
    const q = state.query;
    const rows = state.datasets.filter((d) => {
      if (q && !d.name.toLowerCase().includes(q) && !(d.mountpoint || '').toLowerCase().includes(q)) return false;
      if (state.filter === 'filesystem') return d.kind === 'filesystem';
      if (state.filter === 'volume') return d.kind === 'volume';
      if (state.filter === 'encrypted') return d.encryption && d.encryption !== 'off';
      if (state.filter === 'scheduled') return Boolean(d.snapshotSchedule);
      return true;
    });
    host.querySelector('#nas-ds-hint').textContent = T('datasets.hint', { n: rows.length, total: state.datasets.length });
    table.rows = rows.map((d) => datasetRow(d, state.selected));
  };

  table.rowActions = (row) => {
    if (!screen.isAdmin) return null;
    const d = row._ds;
    const wrap = document.createElement('div');
    wrap.className = 'tf-table__cell-row';
    wrap.innerHTML = `
      <tf-button size="sm" variant="ghost" icon="save" data-act="snap" title="${escapeAttr(T('snapshots.now'))}"></tf-button>
      <tf-button size="sm" variant="ghost" icon="plus" data-act="child" title="${escapeAttr(T('datasets.new_child'))}"></tf-button>
      <tf-button size="sm" variant="ghost" tone="critical" icon="trash" data-act="destroy" title="${escapeAttr(T('datasets.destroy'))}"></tf-button>`;
    wrap.querySelector('[data-act="snap"]').addEventListener('click', (e) => { e.stopPropagation(); openSnapshotNowDialog(screen, { dataset: d.name, onDone: reload }); });
    wrap.querySelector('[data-act="child"]').addEventListener('click', (e) => { e.stopPropagation(); openDatasetCreateDialog(screen, { pool, parent: d.name, kind: 'filesystem', onDone: reload }); });
    wrap.querySelector('[data-act="destroy"]').addEventListener('click', (e) => { e.stopPropagation(); openDatasetDestroyDialog(screen, d, state.datasets, reload); });
    return wrap;
  };
  table.addEventListener('row-click', (e) => {
    const d = e.detail.row._ds;
    state.selected = state.selected === d.name ? null : d.name;
    screen.dataset = state.selected;
    screen.setLocation();
    applyRows();
    drawDetail();
  });
  host.querySelector('[data-act="create"]')?.addEventListener('click', () => openDatasetCreateDialog(screen, { pool, parent: pool, kind: 'filesystem', onDone: reload }));
  host.querySelector('[data-act="create-volume"]')?.addEventListener('click', () => openDatasetCreateDialog(screen, { pool, parent: pool, kind: 'volume', onDone: reload }));

  const drawDetail = () => {
    const el = host.querySelector('#nas-ds-detail');
    if (!state.selected) { el.innerHTML = ''; return; }
    drawDatasetDetail(screen, el, state.selected, reload);
  };

  await reload();
}

const depthOf = (name) => Math.max(0, name.split('/').length - 1);

function datasetRow(d, selected) {
  const depth = depthOf(d.name);
  const short = depth ? d.name.split('/').slice(-1)[0] : d.name;
  const encrypted = d.encryption && d.encryption !== 'off';
  const cap = d.kind === 'volume' ? Number(d.volsizeBytes) || 0 : (Number(d.quotaBytes) || 0) || (Number(d.usedBytes) || 0) + (Number(d.availableBytes) || 0);
  const usedPct = pct(d.usedBytes, cap);
  const tone = usedPct > 90 ? 'critical' : usedPct > 75 ? 'warning' : 'accent';
  return {
    _ds: d,
    _selected: selected === d.name,
    name: `<div class="tf-table__cell-row" style="padding-left:${depth * 18}px">
        <span class="tf-table__cell-title tf-table__cell--mono">${escapeHtml(short)}</span>
        ${d.kind === 'volume' ? `<tf-chip size="sm" status="info" label="ZVOL"></tf-chip>` : ''}
        ${encrypted ? `<tf-chip size="sm" status="${d.keyStatus === 'available' ? 'ok' : 'warn'}" icon="${d.keyStatus === 'available' ? 'unlock' : 'lock'}" label="${escapeAttr(d.keyStatus === 'available' ? T('datasets.key_loaded') : T('datasets.key_locked'))}"></tf-chip>` : ''}
      </div>
      ${depth ? `<div class="tf-table__cell-sub tf-table__cell-sub--mono" style="padding-left:${depth * 18}px">${escapeHtml(d.name)}</div>` : ''}`,
    compression: `<span class="tf-table__cell--mono">${escapeHtml(d.compression || 'off')}</span> ${sourceChipHtml(d.compressionSource)}<div class="tf-table__cell-sub">${escapeHtml(fmtRatio(d.compressRatio))}</div>`,
    quota: d.kind === 'volume' ? fmtBytes(d.volsizeBytes) + (d.thin ? ` (${T('datasets.thin')})` : '') : (d.quotaBytes ? fmtBytes(d.quotaBytes) : '—'),
    usage: `<tf-progress-bar value="${usedPct}" size="sm" tone="${tone}"></tf-progress-bar><div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(fmtBytes(d.usedBytes))} · ${usedPct}%</div>`,
    snapshots: `<span>${Number(d.snapshotCount) || 0}</span>${d.snapshotSchedule ? ` <tf-chip size="sm" status="accent" icon="clock" label="${escapeAttr(fmtSchedule(d.snapshotSchedule.schedule))}"></tf-chip>` : ''}<div class="tf-table__cell-sub">${escapeHtml(fmtBytes(d.snapshotUsedBytes))}</div>`,
    mount: d.kind === 'volume'
      ? `<span class="tf-table__cell-sub tf-table__cell-sub--mono">/dev/zvol/${escapeHtml(d.name)}</span>`
      : `<tf-chip size="sm" status="${d.mounted ? 'ok' : 'warn'}" dot label="${escapeAttr(d.mounted ? T('datasets.mounted') : T('datasets.unmounted'))}"></tf-chip><div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(d.mountpoint || '—')}</div>`,
  };
}

// ---------------------------------------------------------------------------
// Selected dataset: properties and actions
// ---------------------------------------------------------------------------

async function drawDatasetDetail(screen, el, name, onChange) {
  el.innerHTML = `<div class="section-card"><div class="muted">${escapeHtml(I18n.t('common.loading'))}</div></div>`;
  let res;
  try {
    res = await screen.nas('tentaNasDatasetGetRequest', { name });
  } catch (e) {
    el.innerHTML = `<div class="section-card"><div class="num-err">${escapeHtml(errMessage(e))}</div></div>`;
    return;
  }
  if (!el.isConnected) return;
  const d = res.dataset;
  const props = res.properties || [];
  const admin = screen.isAdmin;
  const encrypted = d.encryption && d.encryption !== 'off';
  const actions = [];
  if (admin) {
    actions.push(`<tf-button size="sm" variant="ghost" icon="save" data-act="snap">${escapeHtml(T('snapshots.now'))}</tf-button>`);
    if (encrypted) actions.push(d.keyStatus === 'available'
      ? `<tf-button size="sm" variant="ghost" icon="lock" data-act="key-unload">${escapeHtml(T('datasets.key_unload'))}</tf-button>`
      : `<tf-button size="sm" variant="ghost" icon="unlock" data-act="key-load">${escapeHtml(T('datasets.key_load'))}</tf-button>`);
    if (d.kind === 'filesystem') actions.push(d.mounted
      ? `<tf-button size="sm" variant="ghost" icon="ban" data-act="unmount">${escapeHtml(T('datasets.unmount'))}</tf-button>`
      : `<tf-button size="sm" variant="ghost" icon="play" data-act="mount">${escapeHtml(T('datasets.mount'))}</tf-button>`);
  }
  const summary = [
    [T('datasets.col_usage'), `${fmtBytes(d.usedBytes)} · ${T('datasets.referenced', { v: fmtBytes(d.referencedBytes) })}`],
    [T('datasets.available'), fmtBytes(d.availableBytes)],
    [T('datasets.col_snapshots'), `${Number(d.snapshotCount) || 0} · ${fmtBytes(d.snapshotUsedBytes)}`],
    [T('datasets.created'), d.createdAt ? fmtAgo(d.createdAt) : '—'],
  ];
  el.innerHTML = `
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite(d.kind === 'volume' ? 'database' : 'folder')} <span class="mono">${escapeHtml(d.name)}</span>
          ${d.kind === 'volume' ? `<tf-chip size="sm" status="info" label="ZVOL"></tf-chip>` : ''}
          ${encrypted ? `<tf-chip size="sm" status="${d.keyStatus === 'available' ? 'ok' : 'warn'}" icon="lock" label="${escapeAttr(d.encryption)}"></tf-chip>` : ''}
        </div>
        <div class="actions">${actions.join('')}</div>
      </div>
      <div class="grid-2">
        <div class="stat-rows">${summary.map(([k, v]) => `<div class="sr"><span class="k">${escapeHtml(k)}</span><span class="v">${escapeHtml(v)}</span></div>`).join('')}</div>
        <div class="stat-rows">
          <div class="sr"><span class="k">${escapeHtml(T('datasets.col_mount'))}</span><span class="v mono">${escapeHtml(d.kind === 'volume' ? `/dev/zvol/${d.name}` : (d.mountpoint || '—'))}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('datasets.block_size'))}</span><span class="v mono">${escapeHtml(d.blockSize || '—')}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('datasets.col_quota'))}</span><span class="v mono">${escapeHtml(d.kind === 'volume' ? fmtBytes(d.volsizeBytes) : (d.quotaBytes ? fmtBytes(d.quotaBytes) : T('datasets.no_quota')))}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('datasets.snapshot_schedule'))}</span><span class="v">${d.snapshotSchedule ? `<span class="sched-pill">${sprite('clock')} ${escapeHtml(fmtSchedule(d.snapshotSchedule.schedule))}</span>` : escapeHtml(T('schedule.none'))}</span></div>
        </div>
      </div>
      <tf-table id="nas-ds-props" class="mt-md" empty-message="${escapeAttr(T('props.none'))}">
        <tf-column key="name" label="${escapeAttr(T('props.col_name'))}" renderer="html" width="260"></tf-column>
        <tf-column key="value" label="${escapeAttr(T('props.col_value'))}" renderer="html" fill></tf-column>
        <tf-column key="source" label="${escapeAttr(T('props.col_source'))}" renderer="html" width="140"></tf-column>
      </tf-table>
      ${admin ? `<div class="danger-zone mt-md">${dangerRowHtml({ title: T('datasets.destroy'), desc: T('datasets.destroy_desc', { n: d.snapshotCount }), action: T('datasets.destroy'), act: 'destroy' })}</div>` : ''}
    </div>`;

  const table = el.querySelector('#nas-ds-props');
  const editable = new Set(['compression', 'atime', 'relatime', 'recordsize', 'sync', 'xattr', 'acltype', 'quota', 'refquota', 'reservation', 'mountpoint', 'readonly', 'volsize', 'snapdir', 'exec', 'setuid']);
  table.rowActions = (row) => {
    if (!admin || !editable.has(row._prop.name)) return null;
    const wrap = document.createElement('div');
    wrap.innerHTML = `<tf-button size="sm" variant="ghost" icon="edit" data-act="edit" title="${escapeAttr(I18n.t('common.edit'))}"></tf-button>`;
    wrap.querySelector('[data-act="edit"]').addEventListener('click', (e) => { e.stopPropagation(); openPropertyEditor(screen, d.name, row._prop, onChange, { dataset: true }); });
    return wrap;
  };
  table.rows = props.map((pr) => ({
    _prop: pr,
    name: `<span class="tf-table__cell--mono">${escapeHtml(pr.name)}</span>`,
    value: `<span class="tf-table__cell--mono">${escapeHtml(pr.value ?? '—')}</span>${pr.inheritedFrom ? `<div class="tf-table__cell-sub">${escapeHtml(T('props.inherited_from', { from: pr.inheritedFrom }))}</div>` : ''}`,
    source: sourceChipHtml(pr.source),
  }));

  if (!admin) return;
  el.querySelector('[data-act="snap"]').addEventListener('click', () => openSnapshotNowDialog(screen, { dataset: d.name, onDone: onChange }));
  el.querySelector('[data-act="destroy"]').addEventListener('click', () => openDatasetDestroyDialog(screen, d, [], onChange));
  const simple = async (btnAct, kind, payload, title, done) => {
    el.querySelector(`[data-act="${btnAct}"]`)?.addEventListener('click', async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas(kind, { ...payload, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), title);
      followResponse(screen, res, onChange, done);
    });
  };
  simple('key-load', 'tentaNasDatasetKeyRequest', { name: d.name, action: 'load' }, T('datasets.key_load'), T('datasets.key_loaded_done', { name: d.name }));
  simple('key-unload', 'tentaNasDatasetKeyRequest', { name: d.name, action: 'unload' }, T('datasets.key_unload'), T('datasets.key_unloaded_done', { name: d.name }));
  simple('mount', 'tentaNasDatasetMountRequest', { name: d.name, action: 'mount' }, T('datasets.mount'), T('datasets.mounted_done', { name: d.name }));
  simple('unmount', 'tentaNasDatasetMountRequest', { name: d.name, action: 'unmount' }, T('datasets.unmount'), T('datasets.unmounted_done', { name: d.name }));
}

// ---------------------------------------------------------------------------
// Create dataset / zvol
// ---------------------------------------------------------------------------

const parseSize = (value, unit) => {
  const n = Number(String(value).replace(',', '.'));
  if (!Number.isFinite(n) || n <= 0) return 0;
  return Math.round(n * (unit === 'TiB' ? TIB : GIB));
};

export function openDatasetCreateDialog(screen, { pool, parent, kind = 'filesystem', onDone }) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T(kind === 'volume' ? 'datasets.new_volume' : 'datasets.new'));
  win.setAttribute('subtitle', parent);
  win.setAttribute('icon', kind === 'volume' ? 'database' : 'folder');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '620');
  win.setAttribute('min-width', '480');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const volume = kind === 'volume';
  win.innerHTML = `
    <div slot="body" class="stack">
      <tf-input id="nas-dc-name" label="${escapeAttr(T('datasets.name_label'))}" prefix="${escapeAttr(parent + '/')}" placeholder="${escapeAttr(volume ? 'vm-disk0' : 'media')}" autocomplete="off" spellcheck="false" hint="${escapeAttr(T('datasets.name_hint'))}"></tf-input>
      <div class="form-grid-2">
        <tf-select id="nas-dc-compression" label="${escapeAttr(T('datasets.col_compression'))}"></tf-select>
        <tf-select id="nas-dc-block" label="${escapeAttr(volume ? T('datasets.volblocksize') : T('datasets.recordsize'))}"></tf-select>
      </div>
      <div class="explain-box" id="nas-dc-block-explain">${escapeHtml(T(volume ? 'datasets.volblock_explain' : 'datasets.recordsize_explain'))}</div>
      <div class="form-grid-2">
        <tf-input id="nas-dc-size" type="number" min="0" step="1" inputmode="decimal" label="${escapeAttr(volume ? T('datasets.volsize') : T('datasets.quota'))}" placeholder="${volume ? '100' : ''}" hint="${escapeAttr(volume ? '' : T('datasets.quota_hint'))}"></tf-input>
        <tf-select id="nas-dc-unit" label="${escapeAttr(T('datasets.unit'))}"></tf-select>
      </div>
      ${volume ? `
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('datasets.thin'))}</span><span class="tc-sub">${escapeHtml(T('datasets.thin_sub'))}</span></div>
        <tf-toggle id="nas-dc-thin" checked></tf-toggle>
      </div>` : `
      <div class="form-grid-2">
        <tf-select id="nas-dc-atime" label="atime"></tf-select>
        <tf-select id="nas-dc-sync" label="sync"></tf-select>
      </div>
      <tf-input id="nas-dc-mountpoint" label="${escapeAttr(T('datasets.mountpoint'))}" placeholder="${escapeAttr(T('datasets.mountpoint_inherit'))}" autocomplete="off" spellcheck="false"></tf-input>`}
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('wizard_pool.encryption'))}</span><span class="tc-sub">${escapeHtml(T('datasets.encryption_sub'))}</span></div>
        <tf-toggle id="nas-dc-encryption"></tf-toggle>
      </div>
      <div class="num-err" id="nas-dc-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="plus" data-action="confirm" disabled>${escapeHtml(I18n.t('common.create'))}</tf-button>
    </div>`;
  document.body.appendChild(win);

  const q = (id) => win.querySelector('#' + id);
  q('nas-dc-compression').setOptions(COMPRESSION.map((v) => ({ value: v, label: v === 'inherit' ? T('props.inherit_value') : T('compression.' + v) })), 'inherit');
  q('nas-dc-block').setOptions((volume ? VOLBLOCK : RECORDSIZE).map((v) => ({ value: v, label: v === 'inherit' ? T('props.inherit_value') : v })), volume ? '16K' : 'inherit');
  q('nas-dc-unit').setOptions(['GiB', 'TiB'].map((v) => ({ value: v, label: v })), 'GiB');
  q('nas-dc-atime')?.setOptions(['inherit', 'off', 'on'].map((v) => ({ value: v, label: v === 'inherit' ? T('props.inherit_value') : v })), 'inherit');
  q('nas-dc-sync')?.setOptions(['inherit', 'standard', 'always', 'disabled'].map((v) => ({ value: v, label: v === 'inherit' ? T('props.inherit_value') : v })), 'inherit');

  const btn = win.querySelector('[data-action="confirm"]');
  const nameEl = q('nas-dc-name');
  const valid = () => {
    const short = nameEl.value.trim();
    if (!/^[a-zA-Z0-9_.:-]+$/.test(short)) return false;
    if (volume && parseSize(q('nas-dc-size').value, q('nas-dc-unit').value) <= 0) return false;
    return true;
  };
  let busy = false;
  const sync = () => { if (valid() && !busy) btn.removeAttribute('disabled'); else btn.setAttribute('disabled', ''); };
  for (const id of ['nas-dc-name', 'nas-dc-size']) { q(id).addEventListener('input', sync); q(id).addEventListener('change', sync); }
  q('nas-dc-unit').addEventListener('change', sync);

  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy || !valid()) return;
    busy = true;
    sync();
    const name = `${parent}/${nameEl.value.trim()}`;
    const size = parseSize(q('nas-dc-size').value, q('nas-dc-unit').value);
    const inh = (v) => (v === 'inherit' ? '' : v);
    const payload = {
      name,
      kind,
      compression: inh(q('nas-dc-compression').value),
      blockSize: inh(q('nas-dc-block').value),
      quotaBytes: volume ? 0 : size,
      volsizeBytes: volume ? size : 0,
      thin: volume ? Boolean(q('nas-dc-thin').checked) : false,
      atime: volume ? '' : inh(q('nas-dc-atime').value),
      sync: volume ? '' : inh(q('nas-dc-sync').value),
      encryption: Boolean(q('nas-dc-encryption').checked),
      mountpoint: volume ? '' : q('nas-dc-mountpoint').value.trim(),
    };
    try {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasDatasetCreateRequest', { ...payload, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('datasets.create_title', { name }));
      busy = false;
      if (res === null) { sync(); return; }
      win.close(true);
      followResponse(screen, res, onDone, T('datasets.created_done', { name }));
    } catch (err) {
      busy = false;
      sync();
      const errEl = q('nas-dc-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  return win;
}

// Destroy: children are listed as what goes with a recursive destroy; a
// dataset with children cannot be destroyed without the recursive switch.
export function openDatasetDestroyDialog(screen, dataset, all, onDone) {
  const children = all.filter((d) => d.name.startsWith(dataset.name + '/'));
  const bodyHtml = `
    ${warningHtml('danger', T('datasets.destroy_warning', { name: dataset.name, size: fmtBytes(dataset.usedBytes), n: dataset.snapshotCount }))}
    ${children.length ? `<div class="explain-box">${escapeHtml(T('datasets.destroy_children', { n: children.length }))}</div>
      <ul class="loss-list">${children.slice(0, 8).map((c) => `<li class="ll bad">${sprite('x')}<span class="mono">${escapeHtml(c.name)}</span></li>`).join('')}${children.length > 8 ? `<li class="ll bad">${sprite('x')}<span>${escapeHtml(T('destroy_pool.more', { n: children.length - 8 }))}</span></li>` : ''}</ul>` : ''}
    <tf-checkbox id="nas-dd-recursive" label="${escapeAttr(T('datasets.destroy_recursive'))}" ${children.length ? 'checked' : ''}></tf-checkbox>`;
  return openRetypeDialog({
    title: T('datasets.destroy_title', { name: dataset.name }),
    icon: 'trash',
    name: dataset.name,
    bodyHtml,
    confirmLabel: T('datasets.destroy'),
    onConfirm: async (win) => {
      const recursive = Boolean(win.querySelector('#nas-dd-recursive').checked);
      if (children.length && !recursive) throw new Error(T('datasets.destroy_needs_recursive'));
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasDatasetDestroyRequest', { name: dataset.name, confirmName: dataset.name, recursive, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('datasets.destroy_title', { name: dataset.name }));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('datasets.destroyed_done', { name: dataset.name }));
      return true;
    },
  });
}
