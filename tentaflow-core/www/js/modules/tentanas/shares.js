// ===== File: modules/tentanas/shares.js — the Sharing tab (n12): SMB/NFS shares with per-node fleet mount status, service state, share detail, delete, and the fleet-mounts table of a client node =====
//
// One list request feeds everything the tab shows: the shares of this node
// (with the mount status every other node reported), the two protocol
// services and the share users. A node that only consumes shares from the
// rest of the fleet gets a second table from the fleet-mounts request.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, POLL_POOLS_MS, ADMIN_TIMEOUT_MS, fmtAgo, errMessage } from '/js/modules/tentanas/format.js';
import { openRetypeDialog, followResponse, warningHtml } from '/js/modules/tentanas/dialogs.js';
import { openShareWizard } from '/js/modules/tentanas/share-wizard.js';
import { openShareUsersDialog } from '/js/modules/tentanas/share-users.js';
import '/js/components/tf-table.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-window.js';

// Fleet mount states as `NasMountStatus.state` reports them.
const MOUNT_TONE = { source: 'ok', mounted: 'ok', pending: 'warn', error: 'err', unsupported: 'neutral', disabled: 'neutral' };
export const mountStateTone = (state) => MOUNT_TONE[state] || 'info';
export const mountStateLabel = (state) => (MOUNT_TONE[state] ? T('shares.mount_' + state) : state || '—');
export const protocolLabel = (protocol) => (protocol || '').toUpperCase();
export const protocolChipHtml = (protocol) => `<tf-chip size="sm" status="${protocol === 'nfs' ? 'accent' : 'info'}" label="${escapeAttr(protocolLabel(protocol))}"></tf-chip>`;

/**
 * Folds the per-node mount list into the one chip of the "Na flocie"
 * column: an error wins over a pending node, pending over "all mounted";
 * the tooltip lists every node with its state and detail.
 */
export function fleetSummary(share) {
  const mounts = share.mounts || [];
  const title = mounts.map((m) => `${m.nodeName} ${mountGlyph(m.state)}${m.detail ? ` ${m.detail}` : ''}`).join(' · ');
  if (!share.fleetMount) return { tone: 'neutral', label: T('shares.fleet_off'), title };
  const errors = mounts.filter((m) => m.state === 'error');
  const pending = mounts.filter((m) => m.state === 'pending');
  const mounted = mounts.filter((m) => m.state === 'mounted');
  if (errors.length) return { tone: 'err', label: errors.length === 1 ? T('shares.fleet_error_one', { node: errors[0].nodeName }) : T('shares.fleet_error_many', { n: errors.length }), title };
  if (pending.length) return { tone: 'warn', label: pending.length === 1 ? T('shares.fleet_pending_one', { node: pending[0].nodeName }) : T('shares.fleet_pending_many', { n: pending.length }), title };
  if (mounted.length) return { tone: 'ok', label: T('shares.fleet_mounted', { n: mounted.length }), title };
  return { tone: 'info', label: T('shares.fleet_source_only'), title };
}

function mountGlyph(state) {
  if (state === 'source' || state === 'mounted') return '✓';
  if (state === 'pending') return '⏳';
  if (state === 'error') return '✗';
  return T('shares.mount_na_short');
}

export const mountChipHtml = (state) => `<tf-chip size="sm" status="${mountStateTone(state)}" dot label="${escapeAttr(mountStateLabel(state))}"></tf-chip>`;

// ---------------------------------------------------------------------------
// Tab
// ---------------------------------------------------------------------------

export async function drawShares(screen, body) {
  body.innerHTML = `
    <div class="stack">
      <div class="page-head">
        <div>
          <h1>${sprite('share')} ${escapeHtml(T('shares.title'))}</h1>
          <div class="sub" id="nas-sh-sub">${escapeHtml(I18n.t('common.loading'))}</div>
        </div>
        <div class="actions">
          ${screen.isAdmin ? `<tf-button variant="secondary" icon="users" data-act="users">${escapeHtml(T('shares.users_button'))}</tf-button>` : ''}
          <tf-button variant="primary" icon="plus" data-act="create">${escapeHtml(T('shares.create'))}</tf-button>
        </div>
      </div>
      <div class="section-card" id="nas-sh-services"></div>
      <div class="section-card">
        <div class="tf-toolbar">
          <tf-segmented id="nas-sh-filter" value="all" size="sm">
            <option value="all">${escapeHtml(T('shares.filter_all'))}</option>
            <option value="smb" variant="accent">SMB</option>
            <option value="nfs" variant="accent">NFS</option>
          </tf-segmented>
          <tf-searchbox id="nas-sh-search" placeholder="${escapeAttr(T('shares.search'))}" debounce="150"></tf-searchbox>
          <span class="tf-toolbar-spacer"></span>
          <span class="muted" id="nas-sh-hint"></span>
        </div>
        <div class="section-card-head">
          <div class="title">${sprite('folder')} ${escapeHtml(T('shares.file_shares'))} <tf-chip size="sm" status="neutral" id="nas-sh-count" label="0"></tf-chip></div>
          <span class="hint" id="nas-sh-mount-hint"></span>
        </div>
        <div id="nas-sh-list"></div>
      </div>
      <div class="explain-box">${T('shares.explain', { path: `<span class="mono">${escapeHtml(T('shares.mount_path_pattern', { root: '/mnt/tentanas' }))}</span>` })}</div>
      <div class="section-card" id="nas-sh-fleet" hidden></div>
    </div>`;

  const state = { shares: [], services: [], users: [], mountRoot: '/mnt/tentanas', fleetMounts: [], filter: 'all', query: '', loaded: false, error: '' };

  const refresh = async () => {
    if (screen.disposed || !body.isConnected) return;
    try {
      const [list, fleet] = await Promise.all([
        screen.nas('tentaNasSharesListRequest', {}),
        screen.nas('tentaNasFleetMountsListRequest', {}),
      ]);
      state.shares = (list.shares || []).slice().sort((a, b) => a.name.localeCompare(b.name));
      state.services = list.services || [];
      state.users = list.users || [];
      state.mountRoot = list.mountRoot || state.mountRoot;
      state.fleetMounts = (fleet.mounts || []).slice().sort((a, b) => a.shareName.localeCompare(b.shareName));
      state.error = '';
    } catch (e) {
      state.error = errMessage(e);
      if (!state.loaded) { toast(T('shares.failed', { error: state.error }), 'error'); }
    }
    state.loaded = true;
    if (screen.disposed || !body.isConnected) return;
    paint();
    screen.later(refresh, POLL_POOLS_MS);
  };

  const guardAdmin = () => {
    if (screen.isAdmin) return true;
    toast(T('elevation.admin_only'), 'warning');
    return false;
  };

  const openCreate = () => {
    if (!guardAdmin()) return;
    openShareWizard(screen, { users: state.users, mountRoot: state.mountRoot, onDone: refresh });
  };
  const openEdit = (share) => {
    if (!guardAdmin()) return;
    openShareWizard(screen, { share, users: state.users, mountRoot: state.mountRoot, onDone: refresh });
  };
  const openUsers = () => {
    if (!guardAdmin()) return;
    openShareUsersDialog(screen, { users: state.users, onChange: refresh });
  };
  const refreshMounts = async (share) => {
    try {
      const r = await screen.nas('tentaNasShareMountsRefreshRequest', { shareId: share.shareId });
      const i = state.shares.findIndex((s) => s.shareId === share.shareId);
      if (i >= 0 && r.share) state.shares[i] = r.share;
      toast(T('shares.mounts_refreshed', { name: share.name }), 'success');
      paint();
    } catch (e) {
      toast(errMessage(e), 'error');
    }
  };

  body.querySelector('[data-act="create"]').addEventListener('click', openCreate);
  body.querySelector('[data-act="users"]')?.addEventListener('click', openUsers);
  body.querySelector('#nas-sh-filter').addEventListener('change', (e) => { state.filter = e.detail.value || 'all'; paint(); });
  body.querySelector('#nas-sh-search').addEventListener('search', (e) => { state.query = (e.detail.value || '').trim().toLowerCase(); paint(); });

  const paint = () => {
    paintServices();
    paintList();
    paintFleetMounts();
  };

  const paintServices = () => {
    const el = body.querySelector('#nas-sh-services');
    const smb = state.shares.filter((s) => s.protocol === 'smb').length;
    const nfs = state.shares.filter((s) => s.protocol === 'nfs').length;
    body.querySelector('#nas-sh-sub').textContent = state.shares.length
      ? T('shares.sub', { n: state.shares.length, smb, nfs })
      : T('shares.sub_empty');
    const rows = ['smb', 'nfs'].map((proto) => {
      const svc = state.services.find((s) => s.protocol === proto) || { protocol: proto, installed: false, running: false, version: null, detail: '' };
      const chip = !svc.installed
        ? `<tf-chip size="sm" status="warn" dot label="${escapeAttr(T('shares.service_missing'))}"></tf-chip>`
        : svc.running
          ? `<tf-chip size="sm" status="ok" dot label="${escapeAttr(T('shares.service_running'))}"></tf-chip>`
          : `<tf-chip size="sm" status="warn" dot label="${escapeAttr(T('shares.service_stopped'))}"></tf-chip>`;
      const feature = (screen.environment?.features || []).find((f) => f.id === (proto === 'smb' ? 'samba' : 'nfs'));
      const installable = !svc.installed && screen.isAdmin && feature && (feature.packages || []).length > 0;
      return `
        <div class="sr" data-service="${proto}">
          <span class="k">${escapeHtml(T('shares.service_' + proto))}</span>
          <span class="v">${chip}${svc.version ? `<span class="mono text-3">${escapeHtml(svc.version)}</span>` : ''}${svc.detail ? `<span class="text-3">${escapeHtml(svc.detail)}</span>` : ''}${svc.installed && svc.configPath ? `<span class="mono text-3">${escapeHtml(svc.configPath)}</span>` : ''}
            ${installable ? `<tf-button size="sm" variant="ghost" icon="download" data-act="install" data-feature="${escapeAttr(feature.id)}" ${screen.environment?.packageManager ? '' : `disabled title="${escapeAttr(T('env.package_manager_none'))}"`}>${escapeHtml(T('env.install_sudo'))}</tf-button>` : ''}
            ${!svc.installed && !installable ? `<span class="text-3">${escapeHtml(T('shares.service_install_hint'))}</span>` : ''}
          </span>
        </div>`;
    });
    el.innerHTML = `
      <div class="section-card-head"><div class="title">${sprite('zap')} ${escapeHtml(T('shares.services'))}</div><span class="hint">${escapeHtml(T('shares.services_hint'))}</span></div>
      <div class="stat-rows">${rows.join('')}</div>`;
    for (const b of el.querySelectorAll('[data-act="install"]')) {
      b.addEventListener('click', () => {
        const feature = (screen.environment?.features || []).find((f) => f.id === b.dataset.feature);
        if (feature) screen.installFeature(feature);
      });
    }
  };

  const visibleShares = () => state.shares.filter((s) => {
    if (state.filter !== 'all' && s.protocol !== state.filter) return false;
    if (state.query && !s.name.toLowerCase().includes(state.query) && !(s.sourcePath || '').toLowerCase().includes(state.query)) return false;
    return true;
  });

  const paintList = () => {
    const list = body.querySelector('#nas-sh-list');
    body.querySelector('#nas-sh-count').setAttribute('label', String(state.shares.length));
    body.querySelector('#nas-sh-mount-hint').textContent = T('shares.mount_hint', { root: state.mountRoot });
    if (state.error && !state.shares.length) {
      list.innerHTML = `<div class="num-err">${escapeHtml(state.error)}</div>`;
      return;
    }
    if (!state.shares.length) {
      body.querySelector('#nas-sh-hint').textContent = '';
      list.innerHTML = `
        <tf-empty-state icon="share" title="${escapeAttr(T('shares.empty_title'))}" message="${escapeAttr(T('shares.empty_msg'))}">
          <tf-button variant="primary" icon="plus" data-act="create-empty">${escapeHtml(T('shares.create'))}</tf-button>
        </tf-empty-state>`;
      list.querySelector('[data-act="create-empty"]').addEventListener('click', openCreate);
      return;
    }
    const rows = visibleShares();
    body.querySelector('#nas-sh-hint').textContent = T('shares.hint', { n: rows.length, total: state.shares.length });
    let table = list.querySelector('#nas-sh-table');
    if (!table) {
      list.innerHTML = `
        <tf-table id="nas-sh-table" empty-message="${escapeAttr(T('shares.none_match'))}">
          <tf-column key="name" label="${escapeAttr(T('shares.col_name'))}" renderer="html" fill></tf-column>
          <tf-column key="protocol" label="${escapeAttr(T('shares.col_protocol'))}" renderer="html" nowrap></tf-column>
          <tf-column key="source" label="${escapeAttr(T('shares.col_source'))}" renderer="html" hide-below="900"></tf-column>
          <tf-column key="fleet" label="${escapeAttr(T('shares.col_fleet'))}" renderer="html" nowrap></tf-column>
          <tf-column key="sessions" label="${escapeAttr(T('shares.col_sessions'))}" renderer="num" hide-below="1000"></tf-column>
        </tf-table>`;
      table = list.querySelector('#nas-sh-table');
      table.rowActions = (row) => {
        const s = row._share;
        const wrap = document.createElement('div');
        wrap.className = 'tf-table__cell-row';
        wrap.innerHTML = `
          <tf-button size="sm" variant="ghost" icon="eye" data-act="details" title="${escapeAttr(T('shares.details'))}"></tf-button>
          ${screen.isAdmin ? `
          <tf-button size="sm" variant="ghost" icon="edit" data-act="edit" title="${escapeAttr(T('shares.edit'))}"></tf-button>
          <tf-button size="sm" variant="ghost" icon="refresh" data-act="refresh-mounts" title="${escapeAttr(T('shares.refresh_mounts'))}"></tf-button>
          <tf-button size="sm" variant="ghost" tone="critical" icon="trash" data-act="delete" title="${escapeAttr(T('shares.delete'))}"></tf-button>` : ''}`;
        wrap.querySelector('[data-act="details"]').addEventListener('click', (e) => { e.stopPropagation(); openShareDetail(screen, s.shareId, { mountRoot: state.mountRoot, users: state.users, onChange: refresh }); });
        wrap.querySelector('[data-act="edit"]')?.addEventListener('click', (e) => { e.stopPropagation(); openEdit(s); });
        wrap.querySelector('[data-act="refresh-mounts"]')?.addEventListener('click', (e) => { e.stopPropagation(); refreshMounts(s); });
        wrap.querySelector('[data-act="delete"]')?.addEventListener('click', (e) => { e.stopPropagation(); openShareDeleteDialog(screen, s, refresh); });
        return wrap;
      };
      table.addEventListener('row-click', (e) => openShareDetail(screen, e.detail.row._share.shareId, { mountRoot: state.mountRoot, users: state.users, onChange: refresh }));
    }
    table.rows = rows.map((s) => shareRow(s));
  };

  const paintFleetMounts = () => {
    const el = body.querySelector('#nas-sh-fleet');
    if (!state.fleetMounts.length) { el.hidden = true; el.innerHTML = ''; return; }
    el.hidden = false;
    let table = el.querySelector('#nas-fm-table');
    if (!table) {
      el.innerHTML = `
        <div class="section-card-head">
          <div class="title">${sprite('network')} ${escapeHtml(T('fleet_mounts.title'))} <tf-chip size="sm" status="neutral" id="nas-fm-count" label="0"></tf-chip></div>
          <span class="hint">${escapeHtml(T('fleet_mounts.hint'))}</span>
          ${screen.isAdmin ? `<div class="actions"><tf-button size="sm" variant="ghost" icon="refresh" data-act="retry-all">${escapeHtml(T('fleet_mounts.retry_all'))}</tf-button></div>` : ''}
        </div>
        <tf-table id="nas-fm-table">
          <tf-column key="share" label="${escapeAttr(T('fleet_mounts.col_share'))}" renderer="html" fill></tf-column>
          <tf-column key="source" label="${escapeAttr(T('fleet_mounts.col_source'))}" renderer="html" nowrap></tf-column>
          <tf-column key="mountpoint" label="${escapeAttr(T('fleet_mounts.col_mountpoint'))}" renderer="html" hide-below="900"></tf-column>
          <tf-column key="state" label="${escapeAttr(T('fleet_mounts.col_state'))}" renderer="html" nowrap></tf-column>
          <tf-column key="detail" label="${escapeAttr(T('fleet_mounts.col_detail'))}" renderer="text" hide-below="1000"></tf-column>
        </tf-table>`;
      table = el.querySelector('#nas-fm-table');
      table.rowActions = (row) => {
        if (!screen.isAdmin) return null;
        const m = row._mount;
        const b = document.createElement('tf-button');
        b.setAttribute('size', 'sm');
        b.setAttribute('variant', 'ghost');
        b.setAttribute('icon', 'refresh');
        b.textContent = T('fleet_mounts.retry');
        b.addEventListener('click', (e) => { e.stopPropagation(); retryMount(m.shareId); });
        return b;
      };
      el.querySelector('[data-act="retry-all"]')?.addEventListener('click', () => retryMount(''));
    }
    el.querySelector('#nas-fm-count').setAttribute('label', String(state.fleetMounts.length));
    table.rows = state.fleetMounts.map((m) => ({
      _mount: m,
      share: `<div class="tf-table__cell-row">${sprite('share')}<span class="tf-table__cell-title tf-table__cell--mono">${escapeHtml(m.shareName)}</span>${protocolChipHtml(m.protocol)}</div>`,
      source: `<span class="tf-table__cell--mono">${escapeHtml(m.sourceNodeName)}</span>`,
      mountpoint: `<span class="tf-table__cell--mono">${escapeHtml(m.mountpoint || '—')}</span>`,
      state: `${mountChipHtml(m.state)}${m.checkedAt ? `<div class="tf-table__cell-sub">${escapeHtml(fmtAgo(m.checkedAt))}</div>` : ''}`,
      detail: m.detail || '',
    }));
  };

  // An empty share id retries every pending or failed mount of this node.
  const retryMount = async (shareId) => {
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasFleetMountRetryRequest', { shareId, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('fleet_mounts.retry_title'));
    if (!res) return;
    state.fleetMounts = (res.mounts || []).slice().sort((a, b) => a.shareName.localeCompare(b.shareName));
    toast(T('fleet_mounts.retried'), 'success');
    paintFleetMounts();
  };

  await refresh();
}

function shareRow(s) {
  const fleet = fleetSummary(s);
  const stateChip = s.state === 'error'
    ? `<span title="${escapeAttr(s.stateDetail || '')}"><tf-chip size="sm" status="err" dot label="${escapeAttr(T('shares.state_error'))}"></tf-chip></span>`
    : !s.enabled || s.state === 'disabled'
      ? `<tf-chip size="sm" status="neutral" label="${escapeAttr(T('shares.state_disabled'))}"></tf-chip>`
      : '';
  return {
    _share: s,
    name: `<div class="tf-table__cell-row">${sprite('share')}<span class="tf-table__cell-title tf-table__cell--mono">${escapeHtml(s.name)}</span>${stateChip}</div>${s.stateDetail && s.state === 'error' ? `<div class="tf-table__cell-sub">${escapeHtml(s.stateDetail)}</div>` : ''}`,
    protocol: protocolChipHtml(s.protocol),
    source: `<span class="tf-table__cell--mono">${escapeHtml(s.sourcePath)}</span>${s.dataset ? `<div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(s.dataset)}</div>` : ''}`,
    fleet: `<span title="${escapeAttr(fleet.title)}"><tf-chip size="sm" status="${fleet.tone}" dot label="${escapeAttr(fleet.label)}"></tf-chip></span>`,
    sessions: Number(s.sessions) || 0,
  };
}

// ---------------------------------------------------------------------------
// Share detail window
// ---------------------------------------------------------------------------

export function openShareDetail(screen, shareId, { mountRoot = '/mnt/tentanas', users = [], onChange = null } = {}) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('shares.detail_title'));
  win.setAttribute('icon', 'share');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '760');
  win.setAttribute('min-width', '560');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `<div slot="body" class="stack"><div class="muted">${escapeHtml(I18n.t('common.loading'))}</div></div>
    <div slot="footer"><tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.close'))}</tf-button></div>`;
  document.body.appendChild(win);
  const state = { share: null, sessions: [] };

  const load = async (kind, payload) => {
    try {
      const r = await screen.nas(kind, payload);
      state.share = r.share;
      state.sessions = r.sessions || [];
    } catch (e) {
      if (win.isConnected) win.querySelector('[slot="body"]').innerHTML = `<div class="num-err">${escapeHtml(errMessage(e))}</div>`;
      return false;
    }
    if (win.isConnected) draw();
    return true;
  };

  const draw = () => {
    const s = state.share;
    win.setAttribute('subtitle', `${s.name} · ${protocolLabel(s.protocol)}`);
    const access = s.protocol === 'smb' ? smbAccessRows(s.smb || {}) : nfsAccessRows(s.nfs || {});
    const mounts = s.mounts || [];
    win.innerHTML = `
      <div slot="body" class="stack">
        <div class="row">
          ${protocolChipHtml(s.protocol)}
          ${s.state === 'error' ? `<tf-chip size="sm" status="err" dot label="${escapeAttr(T('shares.state_error'))}"></tf-chip>` : s.enabled ? `<tf-chip size="sm" status="ok" dot label="${escapeAttr(T('shares.state_active'))}"></tf-chip>` : `<tf-chip size="sm" status="neutral" label="${escapeAttr(T('shares.state_disabled'))}"></tf-chip>`}
          ${s.stateDetail ? `<span class="text-3">${escapeHtml(s.stateDetail)}</span>` : ''}
        </div>
        <div class="stat-rows">
          <div class="sr"><span class="k">${escapeHtml(T('shares.col_source'))}</span><span class="v mono">${escapeHtml(s.sourcePath)}${s.dataset ? ` <tf-chip size="sm" status="info" label="${escapeAttr(s.dataset)}"></tf-chip>` : ''}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('shares.fleet_path'))}</span><span class="v mono">${escapeHtml(s.fleetMount ? `${mountRoot}/${s.name}` : T('shares.fleet_off'))}</span></div>
          ${access.map(([k, v]) => `<div class="sr"><span class="k">${escapeHtml(k)}</span><span class="v">${v}</span></div>`).join('')}
          <div class="sr"><span class="k">${escapeHtml(T('shares.created'))}</span><span class="v">${escapeHtml(s.createdAt ? fmtAgo(s.createdAt) : '—')}</span></div>
        </div>
        <div class="section-card-head">
          <div class="title">${sprite('network')} ${escapeHtml(T('shares.fleet_title'))}</div>
          ${screen.isAdmin ? `<div class="actions"><tf-button size="sm" variant="ghost" icon="refresh" data-act="refresh-mounts">${escapeHtml(T('shares.refresh_mounts'))}</tf-button></div>` : ''}
        </div>
        ${mounts.length ? `<div class="stat-rows" id="nas-sd-mounts">${mounts.map((m) => `
          <div class="sr" data-node="${escapeAttr(m.nodeId)}"><span class="k mono">${escapeHtml(m.nodeName)}</span><span class="v">${mountChipHtml(m.state)}${m.mountpoint ? `<span class="mono text-3">${escapeHtml(m.mountpoint)}</span>` : ''}${m.detail ? `<span class="text-3">${escapeHtml(m.detail)}</span>` : ''}</span></div>`).join('')}</div>`
          : `<div class="muted">${escapeHtml(T('shares.fleet_none'))}</div>`}
        <div class="section-card-head"><div class="title">${sprite('users')} ${escapeHtml(T('shares.sessions_title'))} <tf-chip size="sm" status="neutral" label="${state.sessions.length}"></tf-chip></div></div>
        ${state.sessions.length ? `
        <tf-table id="nas-sd-sessions">
          <tf-column key="client" label="${escapeAttr(T('shares.col_client'))}" renderer="html" fill></tf-column>
          <tf-column key="user" label="${escapeAttr(T('shares.col_user'))}" renderer="text"></tf-column>
          <tf-column key="since" label="${escapeAttr(T('shares.col_since'))}" renderer="text" nowrap></tf-column>
        </tf-table>` : `<div class="muted">${escapeHtml(T('shares.sessions_none'))}</div>`}
      </div>
      <div slot="footer">
        ${screen.isAdmin ? `<tf-button variant="ghost" tone="critical" icon="trash" data-act="delete">${escapeHtml(T('shares.delete'))}</tf-button>` : ''}
        <span class="spacer"></span>
        <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.close'))}</tf-button>
        ${screen.isAdmin ? `<tf-button variant="primary" icon="edit" data-act="edit">${escapeHtml(T('shares.edit'))}</tf-button>` : ''}
      </div>`;
    const table = win.querySelector('#nas-sd-sessions');
    if (table) {
      table.rows = state.sessions.map((x) => ({
        client: `<span class="tf-table__cell--mono">${escapeHtml(x.client)}</span>`,
        user: x.user || '—',
        since: x.connectedAt ? fmtAgo(x.connectedAt) : '—',
      }));
    }
    win.querySelector('[data-act="refresh-mounts"]')?.addEventListener('click', async () => {
      if (await load('tentaNasShareMountsRefreshRequest', { shareId })) toast(T('shares.mounts_refreshed', { name: s.name }), 'success');
    });
    win.querySelector('[data-act="edit"]')?.addEventListener('click', () => {
      win.close(true);
      openShareWizard(screen, { share: s, users, mountRoot, onDone: onChange });
    });
    win.querySelector('[data-act="delete"]')?.addEventListener('click', () => {
      win.close(true);
      openShareDeleteDialog(screen, s, onChange);
    });
  };

  win.addEventListener('action', (e) => { if (e.detail?.action === 'cancel') win.close(true); });
  load('tentaNasShareGetRequest', { shareId });
  return win;
}

const onOff = (v) => T(v ? 'shares.on' : 'shares.off');

function smbAccessRows(smb) {
  const grants = (smb.users || []).map((u) => `<span class="mono">${escapeHtml(u.user)}</span> (${escapeHtml(u.mode === 'ro' ? T('shares.mode_ro') : T('shares.mode_rw'))})`).join(' · ');
  return [
    [T('shares.smb_users'), grants || `<span class="text-3">${escapeHtml(T('shares.smb_no_users'))}</span>`],
    [T('shares.smb_options'), escapeHtml([
      `${T('wizard_share.guests')}: ${onOff(smb.guests)}`,
      `${T('wizard_share.previous_versions')}: ${onOff(smb.previousVersions)}`,
      `${T('wizard_share.recycle_bin')}: ${onOff(smb.recycleBin)}`,
      `${T('wizard_share.time_machine')}: ${onOff(smb.timeMachine)}`,
    ].join(' · '))],
  ];
}

function nfsAccessRows(nfs) {
  const nets = (nfs.networks || []).map((n) => `<tf-chip size="sm" status="neutral" label="${escapeAttr(n)}"></tf-chip>`).join('');
  return [
    [T('wizard_share.networks'), nets || `<span class="text-3">${escapeHtml(T('shares.nfs_no_networks'))}</span>`],
    [T('shares.nfs_options'), escapeHtml([
      `${T('wizard_share.read_only')}: ${onOff(nfs.readOnly)}`,
      `root_squash: ${onOff(nfs.rootSquash)}`,
      `${T('wizard_share.async_writes')}: ${onOff(nfs.asyncWrites)}`,
    ].join(' · '))],
  ];
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

// Deleting unexports the share on every node that mounted it; the data
// under the source path stays where it is.
export function openShareDeleteDialog(screen, share, onDone) {
  const mounted = (share.mounts || []).filter((m) => m.state === 'mounted');
  const bodyHtml = `
    ${warningHtml('danger', T('shares.delete_warning', { name: share.name, proto: protocolLabel(share.protocol) }))}
    <ul class="loss-list">
      <li class="ll bad">${sprite('x')}<span>${escapeHtml(T('shares.delete_loss_export', { proto: protocolLabel(share.protocol) }))}</span></li>
      ${mounted.length ? `<li class="ll bad">${sprite('x')}<span>${escapeHtml(T('shares.delete_loss_mounts', { n: mounted.length, nodes: mounted.map((m) => m.nodeName).join(', ') }))}</span></li>` : ''}
      <li class="ll good">${sprite('check')}<span>${escapeHtml(T('shares.delete_keep_data', { path: share.sourcePath }))}</span></li>
    </ul>`;
  return openRetypeDialog({
    title: T('shares.delete_title', { name: share.name }),
    icon: 'trash',
    name: share.name,
    bodyHtml,
    confirmLabel: T('shares.delete'),
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasShareDeleteRequest', { shareId: share.shareId, confirmName: share.name, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('shares.delete_title', { name: share.name }));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('shares.deleted_done', { name: share.name }));
      return true;
    },
  });
}
