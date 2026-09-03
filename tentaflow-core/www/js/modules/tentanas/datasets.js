// ===== File: modules/tentanas/datasets.js — the Datasets inner tab of a pool (n09): dataset tree, per-dataset properties, create/destroy, key and mount actions =====
//
// A pool's datasets form a tree by name (`tank/media/movies`); the table
// shows it as a collapsible tree in one flat list so sorting and searching
// stay simple. Selecting a row loads the full property set for that dataset
// below. Shares of the node are joined in by dataset so a row tells at a
// glance what exports it.

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
import '/js/components/tf-segmented.js';
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
const RECORDSIZE = ['16K', '64K', '128K', '512K', '1M'];
const VOLBLOCK = ['16K', '8K', '32K', '64K', '128K'];
// The "what is it for" presets of the create dialog map a use case to the
// recordsize; `custom` exposes the raw picker.
const PRESETS = { docs: '128K', media: '1M', db: '16K', custom: '' };

export async function drawDatasets(screen, host, { pool, onChange = null }) {
  host.innerHTML = `
    <div class="stack">
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${escapeHtml(T('datasets.title', { pool }))}</div>
          <div class="actions">
            <tf-searchbox id="nas-ds-search" placeholder="${escapeAttr(T('datasets.search'))}" debounce="150"></tf-searchbox>
            ${screen.isAdmin ? `
            <tf-button variant="secondary" size="sm" icon="plus" data-act="create">${escapeHtml(T('datasets.new'))}</tf-button>
            <tf-button variant="secondary" size="sm" icon="database" data-act="create-volume">${escapeHtml(T('datasets.new_volume'))}</tf-button>` : ''}
          </div>
        </div>
        <tf-table id="nas-ds-table" empty-message="${escapeAttr(T('datasets.none'))}">
          <tf-column key="name" label="${escapeAttr(T('datasets.col_name'))}" renderer="html" fill></tf-column>
          <tf-column key="compression" label="${escapeAttr(T('datasets.col_compression'))}" renderer="html" nowrap hide-below="900"></tf-column>
          <tf-column key="quota" label="${escapeAttr(T('datasets.col_quota'))}" renderer="html" nowrap hide-below="1000"></tf-column>
          <tf-column key="usage" label="${escapeAttr(T('datasets.col_usage'))}" renderer="html" width="220"></tf-column>
          <tf-column key="snapshots" label="${escapeAttr(T('datasets.col_snapshots'))}" renderer="html" nowrap hide-below="1100"></tf-column>
          <tf-column key="shares" label="${escapeAttr(T('datasets.col_shares'))}" renderer="html" hide-below="1200"></tf-column>
        </tf-table>
      </div>
      <div id="nas-ds-detail"></div>
    </div>`;

  const state = { pool, datasets: [], shares: [], query: '', collapsed: new Set(), selected: screen.dataset || null };
  const table = host.querySelector('#nas-ds-table');
  host.querySelector('#nas-ds-search').addEventListener('search', (e) => { state.query = (e.detail.value || '').trim().toLowerCase(); applyRows(); });

  const reload = async () => {
    if (screen.disposed || !host.isConnected) return;
    try {
      const [ds, sh] = await Promise.all([
        screen.nas('tentaNasDatasetsListRequest', { pool }),
        screen.nas('tentaNasSharesListRequest', {}),
      ]);
      state.datasets = (ds.datasets || []).slice().sort((a, b) => a.name.localeCompare(b.name));
      state.shares = sh.shares || [];
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
    host.querySelector('#nas-ds-table').rows = treeRows(state).map((d) => datasetRow(d, state));
  };

  table.rowActions = (row) => {
    if (!screen.isAdmin) return null;
    const d = row._ds;
    const root = d.name === pool;
    const wrap = document.createElement('div');
    wrap.className = 'tf-table__cell-row';
    wrap.innerHTML = `
      <tf-button size="sm" variant="ghost" icon="settings" data-act="props" title="${escapeAttr(T('datasets.properties'))}"></tf-button>
      ${root ? '' : `
      <tf-button size="sm" variant="ghost" icon="save" data-act="snap" title="${escapeAttr(T('snapshots.now'))}"></tf-button>
      <tf-button size="sm" variant="ghost" tone="critical" icon="trash" data-act="destroy" title="${escapeAttr(T('datasets.destroy'))}"></tf-button>`}`;
    wrap.querySelector('[data-act="props"]').addEventListener('click', (e) => { e.stopPropagation(); select(d.name, true); });
    wrap.querySelector('[data-act="snap"]')?.addEventListener('click', (e) => { e.stopPropagation(); openSnapshotNowDialog(screen, { dataset: d.name, onDone: reload }); });
    wrap.querySelector('[data-act="destroy"]')?.addEventListener('click', (e) => { e.stopPropagation(); openDatasetDestroyDialog(screen, d, state.datasets, reload); });
    return wrap;
  };
  const select = (name, force = false) => {
    state.selected = !force && state.selected === name ? null : name;
    screen.dataset = state.selected;
    screen.setLocation();
    applyRows();
    drawDetail();
  };
  table.addEventListener('row-click', (e) => select(e.detail.row._ds.name));
  // The tree caret and the navigation chips live inside the table's shadow
  // DOM; catching the click on the host in the capture phase keeps it from
  // reaching the table's own handler, which would turn it into a row-click.
  table.addEventListener('click', (e) => {
    const path = e.composedPath();
    const toggle = path.find((n) => n?.dataset?.treeToggle);
    if (toggle) {
      e.stopPropagation();
      if (state.collapsed.has(toggle.dataset.treeToggle)) state.collapsed.delete(toggle.dataset.treeToggle);
      else state.collapsed.add(toggle.dataset.treeToggle);
      applyRows();
      return;
    }
    const nav = path.find((n) => n?.dataset?.nav);
    if (!nav) return;
    e.stopPropagation();
    if (nav.dataset.nav === 'snapshots') screen.openPool(pool, 'snapshots', nav.dataset.dataset);
    else screen.switchTab('shares');
  }, true);
  host.querySelector('[data-act="create"]')?.addEventListener('click', () => openDatasetCreateDialog(screen, { pool, parent: parentOf(state), kind: 'filesystem', onDone: reload }));
  host.querySelector('[data-act="create-volume"]')?.addEventListener('click', () => openDatasetCreateDialog(screen, { pool, parent: parentOf(state), kind: 'volume', onDone: reload }));

  const drawDetail = () => {
    const el = host.querySelector('#nas-ds-detail');
    if (!state.selected) { el.innerHTML = ''; return; }
    drawDatasetDetail(screen, el, state.selected, reload);
  };

  await reload();
}

// New datasets go under the pool root, or under the selected filesystem —
// the way the mockup prefixes the name with "tank/".
const parentOf = (state) => {
  const sel = state.datasets.find((d) => d.name === state.selected);
  return sel && sel.kind === 'filesystem' ? sel : { name: state.pool, compression: state.datasets.find((d) => d.name === state.pool)?.compression || '' };
};

const depthOf = (name) => Math.max(0, name.split('/').length - 1);

/**
 * The rows of the tree in display order: sorted names already nest, so a
 * row is hidden when any ancestor is collapsed. A search bypasses the
 * folding and matches on the name.
 */
export function treeRows(state) {
  return state.datasets.map((d) => ({ ...d, hasChildren: state.datasets.some((o) => o.name.startsWith(d.name + '/')) })).filter((d) => {
    if (state.query) return d.name.toLowerCase().includes(state.query);
    let cur = d.name;
    while (cur.includes('/')) {
      cur = cur.split('/').slice(0, -1).join('/');
      if (state.collapsed.has(cur)) return false;
    }
    return true;
  });
}

/** Shares that export this dataset (by dataset name or by its mountpoint). */
export const sharesOf = (shares, d) => shares.filter((s) => s.dataset === d.name || (d.mountpoint && s.sourcePath === d.mountpoint));

function datasetRow(d, state) {
  const depth = depthOf(d.name);
  const encrypted = d.encryption && d.encryption !== 'off';
  const root = d.name === state.pool;
  const cap = d.kind === 'volume' ? Number(d.volsizeBytes) || 0 : (Number(d.quotaBytes) || 0) || (Number(d.usedBytes) || 0) + (Number(d.availableBytes) || 0);
  const usedPct = pct(d.usedBytes, cap);
  const tone = usedPct > 90 ? 'critical' : usedPct > 75 ? 'warning' : 'accent';
  const open = !state.collapsed.has(d.name);
  const count = Number(d.snapshotCount) || 0;
  const shares = sharesOf(state.shares, d);
  return {
    _ds: d,
    _selected: state.selected === d.name,
    name: `<div class="tf-table__tree-cell">
        <span class="tf-table__tree-indent" style="width:${depth * 18}px"></span>
        ${d.hasChildren ? `<span class="tf-table__tree-toggle ${open ? 'tf-table__tree-toggle--open' : ''}" data-tree-toggle="${escapeAttr(d.name)}" role="button" aria-expanded="${open}"></span>` : '<span class="tf-table__tree-leaf"></span>'}
        <span class="tf-table__cell--mono"><span class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(d.name)}</span></span>
        ${d.kind === 'volume' ? `<tf-chip size="sm" status="accent" label="ZVOL"></tf-chip>` : ''}
        ${encrypted ? `<tf-chip size="sm" status="${d.keyStatus === 'available' ? 'ok' : 'warn'}" icon="${d.keyStatus === 'available' ? 'unlock' : 'lock'}" label="${escapeAttr(d.keyStatus === 'available' ? T('datasets.key_loaded') : T('datasets.key_locked'))}"></tf-chip>` : ''}
      </div>`,
    compression: `<span class="tf-table__cell--mono">${escapeHtml(d.compression || 'off')}</span>${Number(d.compressRatio) > 1 ? ` <span class="tf-table__cell-sub">(${escapeHtml(fmtRatio(d.compressRatio))})</span>` : ''}`,
    quota: root ? '<span class="tf-table__cell-sub">—</span>'
      : d.kind === 'volume' ? `<span class="tf-table__cell--mono">${escapeHtml(fmtBytes(d.volsizeBytes))}</span>${d.thin ? ` <span class="tf-table__cell-sub">(${escapeHtml(T('datasets.thin_short'))})</span>` : ''}`
        : d.quotaBytes ? `<span class="tf-table__cell--mono">${escapeHtml(fmtBytes(d.quotaBytes))}</span>` : `<span class="tf-table__cell-sub">${escapeHtml(T('datasets.no_quota'))}</span>`,
    usage: `<div class="tf-table__cell-row"><span class="tf-table__cell--mono">${escapeHtml(fmtBytes(d.usedBytes))}</span><tf-progress-bar value="${usedPct}" size="sm" tone="${tone}" style="flex:1"></tf-progress-bar></div>`,
    snapshots: root || (!count && !d.snapshotSchedule)
      ? '<span class="tf-table__cell-sub">—</span>'
      : `<tf-chip size="sm" status="${d.snapshotSchedule ? 'accent' : 'neutral'}" label="${escapeAttr(d.snapshotSchedule ? T('datasets.snapshots_chip', { schedule: fmtSchedule(d.snapshotSchedule.schedule), n: count }) : String(count))}" data-nav="snapshots" data-dataset="${escapeAttr(d.name)}" role="link"></tf-chip>`,
    shares: shares.length
      ? `<div class="tf-table__cell-row">${shares.map((s) => `<tf-chip size="sm" status="info" label="${escapeAttr(T('datasets.share_chip', { protocol: String(s.protocol || '').toUpperCase(), name: s.name }))}" data-nav="shares" role="link"></tf-chip>`).join('')}</div>`
      : '<span class="tf-table__cell-sub">—</span>',
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
  const root = !d.name.includes('/');
  const parent = root ? d.pool : d.name.split('/').slice(0, -1).join('/');
  const encrypted = d.encryption && d.encryption !== 'off';
  const actions = [];
  if (admin) {
    if (!root) actions.push(`<tf-button size="sm" variant="ghost" icon="save" data-act="snap">${escapeHtml(T('snapshots.now'))}</tf-button>`);
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
  const mountHtml = d.kind === 'volume'
    ? `<span class="mono">/dev/zvol/${escapeHtml(d.name)}</span>`
    : `<tf-chip size="sm" status="${d.mounted ? 'ok' : 'warn'}" dot label="${escapeAttr(d.mounted ? T('datasets.mounted') : T('datasets.unmounted'))}"></tf-chip> <span class="mono">${escapeHtml(d.mountpoint || '—')}</span>`;
  el.innerHTML = `
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${escapeHtml(T('datasets.properties'))}: <span class="mono">${escapeHtml(d.name)}</span>
          ${d.kind === 'volume' ? `<tf-chip size="sm" status="accent" label="ZVOL"></tf-chip>` : ''}
          ${encrypted ? `<tf-chip size="sm" status="${d.keyStatus === 'available' ? 'ok' : 'warn'}" icon="lock" label="${escapeAttr(d.encryption)}"></tf-chip>` : ''}
        </div>
        <div class="hint">${escapeHtml(T('datasets.props_hint', { parent }))}</div>
        <div class="actions">${actions.join('')}</div>
      </div>
      <div class="grid-2">
        <div class="stat-rows">${summary.map(([k, v]) => `<div class="sr"><span class="k">${escapeHtml(k)}</span><span class="v">${escapeHtml(v)}</span></div>`).join('')}</div>
        <div class="stat-rows">
          <div class="sr"><span class="k">${escapeHtml(T('datasets.col_mount'))}</span><span class="v">${mountHtml}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('datasets.block_size'))}</span><span class="v mono">${escapeHtml(d.blockSize || '—')}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('datasets.col_quota'))}</span><span class="v mono">${escapeHtml(d.kind === 'volume' ? fmtBytes(d.volsizeBytes) : (d.quotaBytes ? fmtBytes(d.quotaBytes) : T('datasets.no_quota')))}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('datasets.snapshot_schedule'))}</span><span class="v">${d.snapshotSchedule ? `<span class="sched-pill">${sprite('clock')} ${escapeHtml(fmtSchedule(d.snapshotSchedule.schedule))}</span>` : escapeHtml(T('schedule.none'))}</span></div>
        </div>
      </div>
      <tf-table id="nas-ds-props" class="mt-md" empty-message="${escapeAttr(T('props.none'))}">
        <tf-column key="name" label="${escapeAttr(T('props.col_name'))}" renderer="html" width="260"></tf-column>
        <tf-column key="value" label="${escapeAttr(T('props.col_value'))}" renderer="html" fill></tf-column>
        <tf-column key="source" label="${escapeAttr(T('props.col_source'))}" renderer="html" width="160"></tf-column>
      </tf-table>
      ${admin && !root ? `<div class="danger-zone mt-md">${dangerRowHtml({ title: T('datasets.destroy'), desc: T('datasets.destroy_desc', { n: d.snapshotCount }), action: T('datasets.destroy'), act: 'destroy' })}</div>` : ''}
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
    value: `<span class="tf-table__cell--mono">${escapeHtml(pr.value ?? '—')}</span>`,
    source: `${sourceChipHtml(pr.source)}${pr.inheritedFrom ? ` <span class="tf-table__cell-sub">· ${escapeHtml(pr.inheritedFrom)}</span>` : ''}`,
  }));

  if (!admin) return;
  el.querySelector('[data-act="snap"]')?.addEventListener('click', () => openSnapshotNowDialog(screen, { dataset: d.name, onDone: onChange }));
  el.querySelector('[data-act="destroy"]')?.addEventListener('click', () => openDatasetDestroyDialog(screen, d, [], onChange));
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

/**
 * `parent` is the dataset the new one goes under (`{ name, compression }`);
 * a filesystem picks its recordsize through a use-case preset, a zvol asks
 * for size, thin/thick and volblocksize.
 */
export function openDatasetCreateDialog(screen, { pool, parent, kind = 'filesystem', onDone }) {
  const parentName = typeof parent === 'string' ? parent : parent.name;
  const parentCompression = typeof parent === 'string' ? '' : (parent.compression || '');
  const volume = kind === 'volume';
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T(volume ? 'datasets.new_volume' : 'datasets.new'));
  win.setAttribute('icon', volume ? 'database' : 'folder');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '620');
  win.setAttribute('min-width', '480');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <tf-input id="nas-dc-name" label="${escapeAttr(T('datasets.name_label'))}" prefix="${escapeAttr(parentName + '/')}" placeholder="${escapeAttr(volume ? 'vm-disk0' : T('datasets.name_placeholder'))}" autocomplete="off" spellcheck="false"></tf-input>
      ${volume ? `
      <div class="form-grid-2">
        <tf-input id="nas-dc-size" type="number" min="0" step="1" inputmode="decimal" label="${escapeAttr(T('datasets.volsize'))}" placeholder="100"></tf-input>
        <tf-select id="nas-dc-unit" label="${escapeAttr(T('datasets.unit'))}"></tf-select>
      </div>
      <div class="form-grid-2">
        <tf-select id="nas-dc-compression" label="${escapeAttr(T('datasets.col_compression'))}"></tf-select>
        <tf-select id="nas-dc-block" label="${escapeAttr(T('datasets.volblocksize'))}"></tf-select>
      </div>
      <div class="explain-box">${escapeHtml(T('datasets.volblock_explain'))}</div>
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('datasets.thin'))}</span><span class="tc-sub">${escapeHtml(T('datasets.thin_sub'))}</span></div>
        <tf-toggle id="nas-dc-thin" checked></tf-toggle>
      </div>` : `
      <div class="field">
        <label>${escapeHtml(T('datasets.preset_label'))}</label>
        <tf-segmented id="nas-dc-preset" value="docs" size="md">
          <option value="docs">${escapeHtml(T('datasets.preset_docs'))}</option>
          <option value="media">${escapeHtml(T('datasets.preset_media'))}</option>
          <option value="db">${escapeHtml(T('datasets.preset_db'))}</option>
          <option value="custom">${escapeHtml(T('datasets.preset_custom'))}</option>
        </tf-segmented>
        <div class="hint">${escapeHtml(T('datasets.preset_hint'))}</div>
      </div>
      <tf-select id="nas-dc-block" label="${escapeAttr(T('datasets.recordsize'))}" hidden></tf-select>
      <div class="form-grid-2">
        <tf-select id="nas-dc-compression" label="${escapeAttr(T('datasets.col_compression'))}"></tf-select>
        <div class="form-grid-2 nas-dc-quota">
          <tf-input id="nas-dc-size" type="number" min="0" step="1" inputmode="decimal" label="${escapeAttr(T('datasets.quota'))}" placeholder="${escapeAttr(T('datasets.no_quota'))}"></tf-input>
          <tf-select id="nas-dc-unit" label="${escapeAttr(T('datasets.unit'))}"></tf-select>
        </div>
      </div>`}
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('datasets.encryption'))}</span><span class="tc-sub">${escapeHtml(T('datasets.encryption_sub'))}</span></div>
        <tf-toggle id="nas-dc-encryption"></tf-toggle>
      </div>
      ${volume ? '' : `<div class="explain-box">${T('datasets.inherit_explain', { parent: escapeHtml(parentName) })}</div>`}
      <div class="num-err" id="nas-dc-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="plus" data-action="confirm" disabled>${escapeHtml(T(volume ? 'datasets.create_volume' : 'datasets.create'))}</tf-button>
    </div>`;
  document.body.appendChild(win);

  const q = (id) => win.querySelector('#' + id);
  q('nas-dc-compression').setOptions(COMPRESSION.map((v) => ({ value: v, label: v === 'inherit' ? T('datasets.compression_inherit', { parent: parentName, value: parentCompression || T('compression.off') }) : T('compression.' + v) })), 'inherit');
  q('nas-dc-block').setOptions((volume ? VOLBLOCK : RECORDSIZE).map((v) => ({ value: v, label: v })), volume ? '16K' : '128K');
  q('nas-dc-unit').setOptions(['GiB', 'TiB'].map((v) => ({ value: v, label: v })), volume ? 'GiB' : 'TiB');
  q('nas-dc-preset')?.addEventListener('change', (e) => {
    const size = PRESETS[e.detail.value];
    q('nas-dc-block').hidden = Boolean(size);
    if (size) q('nas-dc-block').value = size;
  });

  const btn = win.querySelector('[data-action="confirm"]');
  const nameEl = q('nas-dc-name');
  const valid = () => {
    const short = nameEl.value.trim();
    if (!/^[a-zA-Z0-9_.:-]+(\/[a-zA-Z0-9_.:-]+)*$/.test(short)) return false;
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
    const name = `${parentName}/${nameEl.value.trim()}`;
    const size = parseSize(q('nas-dc-size').value, q('nas-dc-unit').value);
    const compression = q('nas-dc-compression').value;
    const payload = {
      name,
      kind,
      compression: compression === 'inherit' ? '' : compression,
      blockSize: q('nas-dc-block').value,
      quotaBytes: volume ? 0 : size,
      volsizeBytes: volume ? size : 0,
      thin: volume ? Boolean(q('nas-dc-thin').checked) : false,
      atime: '',
      sync: '',
      encryption: Boolean(q('nas-dc-encryption').checked),
      mountpoint: '',
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
