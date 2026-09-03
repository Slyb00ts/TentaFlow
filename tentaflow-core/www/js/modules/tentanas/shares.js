// ===== File: modules/tentanas/shares.js — the Sharing tab (n12): SMB/NFS shares with per-node fleet mount status, pause / resume, share detail and delete =====
//
// One list request feeds the tab: the shares of this node with the mount
// status every other node reported, plus the share users the wizard edits.
// Where this node is a client of a share, its own mount is retried from the
// share detail.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, POLL_POOLS_MS, ADMIN_TIMEOUT_MS, fmtAgo, errMessage, transportLabel, transportChipHtml } from '/js/modules/tentanas/format.js';
import { openRetypeDialog, followResponse, warningHtml } from '/js/modules/tentanas/dialogs.js';
import { openShareWizard } from '/js/modules/tentanas/share-wizard.js';
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

/**
 * "zamontowany · RDMA" — a mounted node names the transport it actually got
 * (§5.5a), every other state has none to name. An older node that reported a
 * mount before the field existed keeps the plain label.
 */
export const mountStateLabel = (state, transport = '') => {
  const label = MOUNT_TONE[state] ? T('shares.mount_' + state) : state || '—';
  if (state !== 'mounted' || (transport !== 'rdma' && transport !== 'tcp')) return label;
  return `${label} · ${transportLabel(transport === 'rdma')}`;
};
export const protocolLabel = (protocol) => (protocol || '').toUpperCase();
export const protocolChipHtml = (protocol) => `<tf-chip size="sm" status="${protocol === 'nfs' ? 'accent' : 'info'}" label="${escapeAttr(protocolLabel(protocol))}"></tf-chip>`;

/**
 * "SMB Direct: bez audytu" (§5.4b). The chip names the transport AND what it
 * costs in the same breath, because the RDMA path is served by ksmbd, which
 * has no access audit — a share can look identical in the list and be
 * unauditable over one of its two addresses.
 */
export const smbDirectChipHtml = (share) => (share.smb?.smbDirect
  ? `<tf-chip size="sm" status="warn" label="${escapeAttr(T('shares.smb_direct_chip'))}"></tf-chip>`
  : '');

/**
 * Folds the per-node mount list into the one chip of the "Na flocie"
 * column: an error wins over a pending node, pending over "all mounted";
 * the tooltip lists every node with its state and detail.
 */
export function fleetSummary(share) {
  const mounts = share.mounts || [];
  const title = mounts.map((m) => `${m.nodeName} ${mountGlyph(m.state)}${m.state === 'mounted' && m.transport ? ` ${transportLabel(m.transport === 'rdma')}` : ''}${m.detail ? ` ${m.detail}` : ''}`).join(' · ');
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

export const mountChipHtml = (state, transport = '') => `<tf-chip size="sm" status="${mountStateTone(state)}" dot label="${escapeAttr(mountStateLabel(state, transport))}"></tf-chip>`;

// ---------------------------------------------------------------------------
// Tab
// ---------------------------------------------------------------------------

export async function drawShares(screen, body) {
  body.innerHTML = `
    <div class="stack">
      <div class="tf-toolbar">
        <span id="nas-sh-filter-host"></span>
        <span class="tf-toolbar-spacer"></span>
        <tf-searchbox id="nas-sh-search" placeholder="${escapeAttr(T('shares.search'))}" debounce="150"></tf-searchbox>
        <tf-button variant="primary" icon="plus" data-act="create">${escapeHtml(T('shares.create'))}</tf-button>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('share')} ${escapeHtml(T('shares.file_shares'))} <tf-chip size="sm" status="neutral" id="nas-sh-count" label="0"></tf-chip></div>
          <span class="hint" id="nas-sh-mount-hint"></span>
        </div>
        <div id="nas-sh-list"></div>
      </div>
      <div class="explain-box" id="nas-sh-explain"></div>
    </div>`;

  const state = { shares: [], users: [], mountRoot: '/mnt/tentanas', filter: 'all', query: '', loaded: false, error: '', counts: '' };

  const refresh = async () => {
    if (screen.disposed || !body.isConnected) return;
    try {
      const list = await screen.nas('tentaNasSharesListRequest', {});
      state.shares = (list.shares || []).slice().sort((a, b) => a.name.localeCompare(b.name));
      state.users = list.users || [];
      state.mountRoot = list.mountRoot || state.mountRoot;
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
  const detailOpts = () => ({ mountRoot: state.mountRoot, users: state.users, onChange: refresh });

  body.querySelector('[data-act="create"]').addEventListener('click', openCreate);
  body.querySelector('#nas-sh-search').addEventListener('search', (e) => { state.query = (e.detail.value || '').trim().toLowerCase(); paintList(); });

  const paint = () => {
    paintFilter();
    paintList();
  };

  // The segmented control reads its options once, so it is rebuilt whenever
  // a count in a label changes (a share added or removed).
  const paintFilter = () => {
    const smb = state.shares.filter((s) => s.protocol === 'smb').length;
    const nfs = state.shares.filter((s) => s.protocol === 'nfs').length;
    const sig = `${state.shares.length}/${smb}/${nfs}`;
    if (sig === state.counts) return;
    state.counts = sig;
    const host = body.querySelector('#nas-sh-filter-host');
    host.innerHTML = `
      <tf-segmented id="nas-sh-filter" value="${escapeAttr(state.filter)}" size="sm">
        <option value="all">${escapeHtml(T('shares.filter_all', { n: state.shares.length }))}</option>
        <option value="smb">SMB ${smb}</option>
        <option value="nfs">NFS ${nfs}</option>
      </tf-segmented>`;
    host.querySelector('#nas-sh-filter').addEventListener('change', (e) => { state.filter = e.detail.value || 'all'; paintList(); });
  };

  const visibleShares = () => state.shares.filter((s) => {
    if (state.filter !== 'all' && s.protocol !== state.filter) return false;
    if (state.query && !s.name.toLowerCase().includes(state.query) && !(s.sourcePath || '').toLowerCase().includes(state.query) && !(s.dataset || '').toLowerCase().includes(state.query)) return false;
    return true;
  });

  const paintList = () => {
    const list = body.querySelector('#nas-sh-list');
    const pattern = `<span class="mono">${escapeHtml(T('shares.mount_path_pattern', { root: state.mountRoot }))}</span>`;
    body.querySelector('#nas-sh-count').setAttribute('label', String(state.shares.length));
    body.querySelector('#nas-sh-mount-hint').innerHTML = T('shares.mount_hint', { path: pattern });
    body.querySelector('#nas-sh-explain').innerHTML = T('shares.explain', { path: pattern });
    if (state.error && !state.shares.length) {
      list.innerHTML = `<div class="num-err">${escapeHtml(state.error)}</div>`;
      return;
    }
    if (!state.shares.length) {
      list.innerHTML = `
        <tf-empty-state icon="share" title="${escapeAttr(T('shares.empty_title'))}" message="${escapeAttr(T('shares.empty_msg'))}">
          <tf-button variant="primary" icon="plus" data-act="create-empty">${escapeHtml(T('shares.create'))}</tf-button>
        </tf-empty-state>`;
      list.querySelector('[data-act="create-empty"]').addEventListener('click', openCreate);
      return;
    }
    let table = list.querySelector('#nas-sh-table');
    if (!table) {
      list.innerHTML = `
        <tf-table id="nas-sh-table" actions-label="${escapeAttr(I18n.t('common.actions'))}" empty-message="${escapeAttr(T('shares.none_match'))}">
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
        wrap.innerHTML = screen.isAdmin ? `
          <tf-button size="sm" variant="ghost" icon="edit" data-act="edit" title="${escapeAttr(T('shares.edit'))}"></tf-button>
          <tf-button size="sm" variant="ghost" icon="${s.enabled ? 'pause' : 'play'}" data-act="pause" title="${escapeAttr(s.enabled ? T('shares.pause') : T('shares.resume'))}"></tf-button>
          <tf-button size="sm" variant="ghost" tone="critical" icon="trash" data-act="delete" title="${escapeAttr(T('shares.delete'))}"></tf-button>`
          : `<tf-button size="sm" variant="ghost" icon="eye" data-act="details" title="${escapeAttr(T('shares.details'))}"></tf-button>`;
        wrap.querySelector('[data-act="details"]')?.addEventListener('click', (e) => { e.stopPropagation(); openShareDetail(screen, s.shareId, detailOpts()); });
        wrap.querySelector('[data-act="edit"]')?.addEventListener('click', (e) => { e.stopPropagation(); openEdit(s); });
        wrap.querySelector('[data-act="pause"]')?.addEventListener('click', (e) => { e.stopPropagation(); setShareEnabled(screen, s, !s.enabled, refresh); });
        wrap.querySelector('[data-act="delete"]')?.addEventListener('click', (e) => { e.stopPropagation(); openShareDeleteDialog(screen, s, refresh); });
        return wrap;
      };
      table.addEventListener('row-click', (e) => openShareDetail(screen, e.detail.row._share.shareId, detailOpts()));
    }
    table.rows = visibleShares().map((s) => shareRow(s));
  };

  await refresh();
}

/**
 * "Zatrzymaj udostępnianie": the share keeps its options and its fleet
 * mounts configuration, only `enabled` flips — the same update the wizard
 * sends, so resuming later restores exactly what was there.
 */
export async function setShareEnabled(screen, share, enabled, onDone) {
  const title = enabled ? T('shares.resume_title', { name: share.name }) : T('shares.pause_title', { name: share.name });
  const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasShareUpdateRequest', {
    shareId: share.shareId,
    smb: share.smb || null,
    nfs: share.nfs || null,
    fleetMount: Boolean(share.fleetMount),
    enabled,
    sudoPassword,
  }, { timeoutMs: ADMIN_TIMEOUT_MS }), title);
  followResponse(screen, res, onDone, enabled ? T('shares.resumed_done', { name: share.name }) : T('shares.paused_done', { name: share.name }));
}

function shareRow(s) {
  const fleet = fleetSummary(s);
  // An ACTIVE share with a detail is a warning the node reported while
  // applying — today only the SMB Direct refusal of §5.4b. It gets its own
  // chip: an option the admin turned on that did not take effect must never
  // read the same as one that did.
  const stateChip = s.state === 'error'
    ? `<span title="${escapeAttr(s.stateDetail || '')}"><tf-chip size="sm" status="err" dot label="${escapeAttr(T('shares.state_error'))}"></tf-chip></span>`
    : !s.enabled || s.state === 'disabled'
      ? `<tf-chip size="sm" status="neutral" label="${escapeAttr(T('shares.state_disabled'))}"></tf-chip>`
      : s.stateDetail
        ? `<span title="${escapeAttr(s.stateDetail)}"><tf-chip size="sm" status="warn" dot label="${escapeAttr(T('shares.state_warning'))}"></tf-chip></span>`
        : '';
  return {
    _share: s,
    name: `<div class="tf-table__cell-row">${sprite('share')}<span class="tf-table__cell--mono"><span class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(s.name)}</span></span>${stateChip}</div>${s.stateDetail ? `<div class="tf-table__cell-sub">${escapeHtml(s.stateDetail)}</div>` : ''}`,
    // The transport chip only marks the non-default: every share serves TCP,
    // and the detail window names both for whichever one is open.
    protocol: `${protocolChipHtml(s.protocol)}${s.nfs?.rdma ? ` ${transportChipHtml('rdma')}` : ''}${s.smb?.smbDirect ? ` ${smbDirectChipHtml(s)}` : ''}`,
    source: `<span class="tf-table__cell--mono">${escapeHtml(s.dataset || s.sourcePath)}</span>${s.dataset ? `<div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(s.sourcePath)}</div>` : ''}`,
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
  const localNodeId = screen.currentNode()?.nodeId || '';

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
          ${smbDirectChipHtml(s)}
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
          <div class="sr" data-node="${escapeAttr(m.nodeId)}"><span class="k mono">${escapeHtml(m.nodeName)}</span><span class="v">${mountChipHtml(m.state, m.transport)}${m.mountpoint ? `<span class="mono text-3">${escapeHtml(m.mountpoint)}</span>` : ''}${m.detail ? `<span class="text-3">${escapeHtml(m.detail)}</span>` : ''}${
            screen.isAdmin && m.nodeId === localNodeId && (m.state === 'pending' || m.state === 'error') ? `<tf-button size="sm" variant="ghost" icon="refresh" data-act="retry-mount">${escapeHtml(T('shares.retry_mount'))}</tf-button>` : ''}</span></div>`).join('')}</div>`
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
        ${screen.isAdmin ? `<tf-button variant="ghost" tone="critical" icon="trash" data-act="delete">${escapeHtml(T('shares.delete'))}</tf-button>
        <tf-button variant="ghost" icon="${s.enabled ? 'pause' : 'play'}" data-act="pause">${escapeHtml(s.enabled ? T('shares.pause') : T('shares.resume'))}</tf-button>` : ''}
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
    // This node mounts the share as a client: a pending or failed mount can
    // be retried from here once the elevation channel is armed.
    win.querySelector('[data-act="retry-mount"]')?.addEventListener('click', async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasFleetMountRetryRequest', { shareId, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('shares.retry_mount_title', { name: s.name }));
      if (!res) return;
      toast(T('shares.retried_mount', { name: s.name }), 'success');
      await load('tentaNasShareGetRequest', { shareId });
      if (onChange) onChange();
    });
    win.querySelector('[data-act="edit"]')?.addEventListener('click', () => {
      win.close(true);
      openShareWizard(screen, { share: s, users, mountRoot, onDone: onChange });
    });
    win.querySelector('[data-act="delete"]')?.addEventListener('click', () => {
      win.close(true);
      openShareDeleteDialog(screen, s, onChange);
    });
    win.querySelector('[data-act="pause"]')?.addEventListener('click', async () => {
      await setShareEnabled(screen, s, !s.enabled, onChange);
      if (win.isConnected) load('tentaNasShareGetRequest', { shareId });
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
    // Always named, both ways, like the NFS transport below: which SMB
    // backends serve a share is a decision, not a detail that only shows up
    // when it is unusual (§5.4b).
    [T('wizard_share.smb_direct'), smb.smbDirect
      ? `<tf-chip size="sm" status="warn" label="${escapeAttr(T('shares.smb_direct_chip'))}"></tf-chip>`
      : `<tf-chip size="sm" status="neutral" label="${escapeAttr(T('shares.smb_direct_off'))}"></tf-chip>`],
    [T('shares.smb_options'), escapeHtml([
      `${T('wizard_share.guests')}: ${onOff(smb.guests)}`,
      `${T('wizard_share.previous_versions')}: ${onOff(smb.previousVersions)}`,
      `${T('wizard_share.recycle_bin')}: ${onOff(smb.recycleBin)}`,
      `${T('wizard_share.time_machine')}: ${onOff(smb.timeMachine)}`,
    ].join(' · '))],
    // The audit is named both ways too, and a share that audits while also
    // serving SMB Direct says which half of it is not audited (§5.4b/§5.10).
    [T('wizard_share.audit'), smb.audit
      ? `<tf-chip size="sm" status="ok" label="${escapeAttr(auditSummary(smb))}"></tf-chip>${
        smb.smbDirect ? `<div class="tf-table__cell-sub">${escapeHtml(T('shares.audit_smb_direct'))}</div>` : ''}`
      : `<tf-chip size="sm" status="neutral" label="${escapeAttr(T('shares.off'))}"></tf-chip>`],
  ];
}

/// "sukces + odmowa · zapisy, uprawnienia" — what the share actually audits,
/// read from the same three fields the node turns into the config lines.
export function auditSummary(smb) {
  const results = [
    smb.auditSuccess ? T('wizard_share.audit_result_success') : '',
    smb.auditFailure ? T('wizard_share.audit_result_failure') : '',
  ].filter(Boolean).join(' + ');
  const groups = (smb.auditGroups || []).map((g) => T('wizard_share.audit_group_' + g)).join(', ');
  return [results, groups].filter(Boolean).join(' · ') || T('shares.audit_nothing');
}

function nfsAccessRows(nfs) {
  const nets = (nfs.networks || []).map((n) => `<tf-chip size="sm" status="neutral" label="${escapeAttr(n)}"></tf-chip>`).join('');
  return [
    [T('wizard_share.networks'), nets || `<span class="text-3">${escapeHtml(T('shares.nfs_no_networks'))}</span>`],
    // Always named, both ways: the transport is a decision, not a detail that
    // only shows up when it is unusual (§5.5a).
    [T('wizard_share.transport'), transportChipHtml(nfs.rdma ? 'rdma' : 'tcp')],
    [T('shares.nfs_options'), escapeHtml([
      `${T('wizard_share.read_only')}: ${onOff(nfs.readOnly)}`,
      `root_squash: ${onOff(nfs.rootSquash)}`,
      `${T('wizard_share.async_writes')}: ${onOff(nfs.asyncWrites)}`,
    ].join(' · '))],
    // An audited export gets auditd watches, and its events go to the HOST's
    // audit log, not to the app's access log (§5.10) — said here so nobody
    // looks for them in "Dziennik dostępu".
    [T('wizard_share.audit'), nfs.audit
      ? `<tf-chip size="sm" status="ok" label="${escapeAttr(T('shares.audit_auditd'))}"></tf-chip><div class="tf-table__cell-sub">${escapeHtml(T('shares.audit_auditd_note'))}</div>`
      : `<tf-chip size="sm" status="neutral" label="${escapeAttr(T('shares.off'))}"></tf-chip>`],
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
