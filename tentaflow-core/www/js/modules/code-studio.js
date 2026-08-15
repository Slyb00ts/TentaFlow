// ===== File: code-studio.js — Code Studio: workspace registry, wizard, settings, session shell =====
//
// The module owns three routes:
//   #code-studio                              → workspace list (W01)
//   #code-studio/<workspaceId>                → workspace: sessions + settings
//   #code-studio/<workspaceId>/<sessionId>    → session console (K01 shell)
//
// It renders the SHELL of the session console — the four state attributes on
// `.cs-shell`, the workspace bar, the phone sheet and its scrim — and hands the
// session surface to code-studio-session.js. The dock/stage panes belong to
// code-studio-panes.js; this file never touches them.
//
// Everything crosses the binary protocol (ApiBinary + codec `codeStudio*`).
// Every visible string comes from i18n `code_studio.*`; every primitive is a
// `tf-*` component.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast, formatBytes, formatRelative } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { attachSession as attachConnection, detachSession as detachConnection } from '/js/modules/code-studio-connection.js';
import { rememberNodeName } from '/js/modules/connection-overlay.js';
import '/js/components/tf-button.js';
import '/js/components/tf-choice-card.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-table.js';
import '/js/components/tf-menu.js';
import '/js/components/tf-window.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-progress-bar.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';

// Autonomy modes ordered by capability (§9.5). `autonomous` has no enforcement
// point in native mode, so the wizard removes it there and the server rejects
// it independently.
const AUTONOMY_MODES = ['plan', 'normal', 'auto_edit', 'autonomous'];
const EGRESS_POLICIES = ['local_only', 'org_approved', 'any'];
const MEMBER_ROLES = ['owner', 'editor', 'viewer'];
const AUTH_KINDS = ['none', 'token', 'ssh_key'];

// Combinations a `trusted_native` workspace cannot keep (§7.1, §9.5).
const NATIVE_BLOCKED_AUTONOMY = 'autonomous';
const NATIVE_BLOCKED_EGRESS = 'local_only';

// Capabilities that may carry a workspace-wide `always` allowlist entry (§9.3).
const ALLOWLIST_CAPABILITIES = ['exec', 'net_egress', 'fs_write'];

const LIST_FILTERS = ['all', 'with_session', 'attention', 'container', 'archived'];

// Below this width the table drops the columns that only add noise on a phone.
const NARROW_QUERY = '(max-width: 900px)';
// tf-table's own card breakpoint. Every remaining column becomes a captioned
// block there, so a three-column row costs ~240px — one record per screen. One
// composed column instead keeps a record to a single block.
const CARD_QUERY = '(max-width: 720px)';

const state = {
  isAdmin: false,
  // 'list' | 'workspace' | 'session'
  view: 'list',
  workspaces: [],
  nodes: [],
  canCreate: false,
  includeArchived: false,
  listFilter: 'all',
  search: '',
  nodeFilter: 'all',
  workspaceId: null,
  sessionId: null,
  workspace: null,
  members: [],
  provisioning: [],
  sessions: [],
  allowlist: [],
  indexStatus: null,
  provisionTimer: null,
  sessionModule: null,
  narrow: false,
  card: false,
  narrowListener: null,
  cardListener: null,
  hashListener: null,
  wins: new Set(),
};

// =============================================================================
// Small helpers
// =============================================================================

function t(key, params) {
  return I18n.t(`code_studio.${key}`, params);
}

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function wsId(w) {
  return String(w?.workspaceId ?? w?.workspace_id ?? '');
}

function shortId(id) {
  const text = String(id ?? '');
  return text.length > 8 ? `${text.slice(0, 8)}…` : text;
}

function isNative(workspace) {
  return (workspace?.execMode ?? workspace?.exec_mode) === 'trusted_native';
}

function enforcementOf(workspace) {
  return String(workspace?.egressEnforcement ?? workspace?.egress_enforcement ?? 'unrestricted');
}

function statusOf(workspace) {
  return String(workspace?.status ?? 'active');
}

function myRole(workspace) {
  return String(workspace?.myRole ?? workspace?.my_role ?? 'viewer');
}

function canManage(workspace) {
  return myRole(workspace) === 'owner' || state.isAdmin;
}

function quotaBytes(workspace) {
  const raw = workspace?.quotaDiskBytes ?? workspace?.quota_disk_bytes;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? n : 0;
}

// The protocol sends SQLite `YYYY-MM-DD HH:MM:SS` in UTC, which no Date parser
// reads without the marker.
function parseTimestamp(value) {
  if (!value) return null;
  const text = String(value);
  const date = new Date(text.includes('T') ? text : `${text.replace(' ', 'T')}Z`);
  return Number.isNaN(date.getTime()) ? null : date;
}

function formatTimestamp(value) {
  const date = parseTimestamp(value);
  if (!date) return value ? String(value) : '—';
  return date.toLocaleString(I18n.getLanguage() || undefined, {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit',
  });
}

// An activity column exists to answer "how fresh is this", and an absolute stamp
// makes the reader do the subtraction — in a fixture where every row was touched
// in the same minute it also renders four identical strings.
function relativeTimestamp(value) {
  const date = parseTimestamp(value);
  return date ? formatRelative(Math.floor(date.getTime() / 1000)) : '—';
}

function describeError(err) {
  const message = err?.message ? String(err.message) : '';
  return message || t('error_generic');
}

function reportError(err) {
  toast(describeError(err), 'error');
}

// =============================================================================
// Protocol calls — binary only, no REST anywhere in this module
// =============================================================================

async function fetchWorkspaces() {
  const body = await ApiBinary.one('codeStudioWorkspacesListRequest', {
    includeArchived: state.includeArchived,
  });
  state.workspaces = Array.isArray(body.workspaces) ? body.workspaces : [];
  state.nodes = Array.isArray(body.nodes) ? body.nodes : [];
  state.canCreate = !!(body.canCreate ?? body.can_create);
  // The registry names this machine; the disconnection overlay cannot ask for
  // that name when it is drawn, so it is handed over while the socket is alive.
  const local = state.nodes.find((n) => (n.isLocal ?? n.is_local));
  if (local) rememberNodeName(local.name ?? local.hostname);
}

async function fetchWorkspace(workspaceId) {
  const body = await ApiBinary.one('codeStudioWorkspaceGetRequest', { workspaceId });
  state.workspace = body.workspace ?? null;
  state.members = Array.isArray(body.members) ? body.members : [];
  state.provisioning = Array.isArray(body.provisioning) ? body.provisioning : [];
  return state.workspace;
}

async function fetchSessions(workspaceId) {
  const body = await ApiBinary.one('codeStudioSessionsListRequest', { workspaceId });
  state.sessions = Array.isArray(body.sessions) ? body.sessions : [];
  return state.sessions;
}

async function fetchAllowlist(workspaceId) {
  const body = await ApiBinary.one('codeStudioWorkspaceAllowlistListRequest', { workspaceId });
  state.allowlist = Array.isArray(body.entries) ? body.entries : [];
  return state.allowlist;
}

async function fetchIndexStatus(workspaceId) {
  const body = await ApiBinary.one('codeStudioIndexStatusRequest', { workspaceId });
  state.indexStatus = body;
  return body;
}

// =============================================================================
// Routing — three hash routes, resolved inside the module
// =============================================================================

function parseHash() {
  const raw = String(window.location.hash || '').replace(/^#/, '');
  const parts = raw.split('/').filter(Boolean).map((p) => {
    try { return decodeURIComponent(p); } catch { return p; }
  });
  if (parts[0] !== 'code-studio') return null;
  return { workspaceId: parts[1] || null, sessionId: parts[2] || null };
}

function writeHash(workspaceId, sessionId) {
  let next = '#code-studio';
  if (workspaceId) next += `/${encodeURIComponent(workspaceId)}`;
  if (workspaceId && sessionId) next += `/${encodeURIComponent(sessionId)}`;
  if (window.location.hash !== next) {
    window.history.replaceState(null, '', next);
  }
}

/** The single entry point for switching between the three views. */
async function goto(workspaceId, sessionId) {
  const nextWorkspace = workspaceId || null;
  const nextSession = nextWorkspace ? (sessionId || null) : null;
  if (state.workspaceId === nextWorkspace && state.sessionId === nextSession && state.view !== 'list') {
    writeHash(nextWorkspace, nextSession);
    return;
  }

  stopProvisionTracking();
  const leavingSession = state.view === 'session'
    && (state.workspaceId !== nextWorkspace || state.sessionId !== nextSession);
  if (leavingSession) await unmountSessionSurface();

  state.workspaceId = nextWorkspace;
  state.sessionId = nextSession;
  writeHash(nextWorkspace, nextSession);

  if (!nextWorkspace) {
    state.view = 'list';
    showView('list');
    await refreshList();
    return;
  }
  if (nextSession) {
    state.view = 'session';
    showView('session');
    await enterSession(nextWorkspace, nextSession);
    return;
  }
  state.view = 'workspace';
  showView('workspace');
  await enterWorkspace(nextWorkspace);
}

function showView(view) {
  const map = {
    list: byId('cs-list-view'),
    workspace: byId('cs-workspace-view'),
    session: byId('cs-session-view'),
  };
  Object.entries(map).forEach(([name, el]) => {
    if (el) el.hidden = name !== view;
  });
}

// =============================================================================
// W01 — workspace list
// =============================================================================

function listShellHtml() {
  return `
    <div class="tf-toolbar cs-listbar">
      <div class="cs-listbar-title">
        <h1>${sprite('terminal')}${escapeHtml(t('title'))}</h1>
        <div class="sub" id="cs-list-sub"></div>
      </div>
      <tf-searchbox id="cs-search" placeholder="${escapeAttr(t('search_placeholder'))}" debounce="200"></tf-searchbox>
      <tf-select id="cs-node-filter" value="all"></tf-select>
      <tf-button variant="outline" icon="refresh" id="cs-refresh">${escapeHtml(t('action_refresh'))}</tf-button>
      <tf-button variant="primary" icon="plus" id="cs-new">${escapeHtml(t('new_workspace'))}</tf-button>
    </div>
    <div class="cs-kpi-grid" id="cs-kpis"></div>
    <tf-filter-chips class="cs-filter-row" id="cs-filter" mode="single" scroll></tf-filter-chips>
    <div id="cs-table-host"></div>
    <div class="cs-table-footer" id="cs-table-footer"></div>
  `;
}

function filteredWorkspaces() {
  const query = state.search.trim().toLowerCase();
  return state.workspaces.filter((w) => {
    const status = statusOf(w);
    if (state.listFilter === 'archived' && status !== 'archived') return false;
    if (state.listFilter !== 'archived' && status === 'archived') return false;
    if (state.listFilter === 'with_session' && Number(w.openSessions ?? w.open_sessions ?? 0) === 0) return false;
    if (state.listFilter === 'attention' && status !== 'error' && status !== 'provisioning') return false;
    if (state.listFilter === 'container' && isNative(w)) return false;
    if (state.nodeFilter !== 'all' && String(w.nodeId ?? w.node_id) !== state.nodeFilter) return false;
    if (!query) return true;
    const haystack = [
      w.name,
      w.slug,
      w.nodeName ?? w.node_name,
      w.repoUrl ?? w.repo_url,
      wsId(w),
    ].map((v) => String(v ?? '').toLowerCase()).join(' ');
    return haystack.includes(query);
  });
}

function renderKpis() {
  const host = byId('cs-kpis');
  if (!host) return;
  const all = state.workspaces;
  const active = all.filter((w) => statusOf(w) === 'active').length;
  const archived = all.filter((w) => statusOf(w) === 'archived').length;
  const sessions = all.reduce((sum, w) => sum + Number(w.openSessions ?? w.open_sessions ?? 0), 0);
  const withSessions = all.filter((w) => Number(w.openSessions ?? w.open_sessions ?? 0) > 0).length;
  const attention = all.filter((w) => statusOf(w) === 'error').length;
  const provisioning = all.filter((w) => statusOf(w) === 'provisioning').length;
  // Usage is not in the list payload (see quotaCell), so the tile reports the
  // granted quota — a number the listing actually carries.
  const quota = all.reduce((sum, w) => sum + quotaBytes(w), 0);
  const withoutQuota = all.filter((w) => !quotaBytes(w)).length;

  const cards = [
    {
      label: t('kpi_workspaces'),
      value: String(active),
      delta: t('kpi_workspaces_delta', { count: archived }),
      deltaType: 'neutral',
    },
    {
      label: t('kpi_sessions'),
      value: String(sessions),
      delta: t('kpi_sessions_delta', { count: withSessions }),
      deltaType: 'neutral',
      accent: sessions > 0 ? 'info' : '',
    },
    {
      label: t('kpi_attention'),
      value: String(attention),
      delta: t('kpi_attention_delta', { count: provisioning }),
      deltaType: attention > 0 ? 'warn' : 'neutral',
      accent: attention > 0 ? 'danger' : '',
    },
  ];
  // A tile reading "—" is a quarter of the KPI row spent saying nothing. The
  // quota column already disappears when no workspace carries a limit, and the
  // tile follows the same rule instead of contradicting it.
  if (quota > 0) {
    cards.push({
      label: t('kpi_quota'),
      value: formatBytes(quota),
      delta: t('kpi_quota_delta', { count: withoutQuota }),
      deltaType: 'neutral',
    });
  }

  host.replaceChildren(...cards.map((c) => {
    const el = document.createElement('tf-stat-card');
    el.setAttribute('label', c.label);
    el.setAttribute('value', c.value);
    el.setAttribute('delta', c.delta);
    el.setAttribute('delta-type', c.deltaType);
    if (c.accent) el.setAttribute('accent', c.accent);
    return el;
  }));
}

function syncNodeFilter() {
  const select = byId('cs-node-filter');
  if (!select) return;
  const options = [{ value: 'all', label: t('filter_node_any') }];
  for (const node of state.nodes) {
    const id = String(node.nodeId ?? node.node_id ?? '');
    options.push({ value: id, label: String(node.name ?? id) });
  }
  select.setOptions(options, state.nodeFilter);
}

function syncFilterChips() {
  const chips = byId('cs-filter');
  if (!chips) return;
  chips.filters = LIST_FILTERS.map((id) => ({
    id,
    label: t(`chip_${id}`),
    active: id === state.listFilter,
  }));
}

function modeChip(workspace) {
  return isNative(workspace)
    ? { status: 'warn', label: t('mode_native'), dot: true }
    : { status: 'ok', label: t('mode_container'), dot: true };
}

// A badge has to carry a signal. "No sessions" is the absence of one, so only
// the states that ask for attention keep the chip form; the rest reads as the
// same two-line cell the other columns use. The chip markup is the one tf-table
// renders itself — controls.css is adopted into its shadow root, so it styles.
//
// A cell that says "—" over "no sessions" states the same absence twice; the
// dash is dropped and the words carry it alone.
function sessionCell(workspace) {
  const status = statusOf(workspace);
  const open = Number(workspace.openSessions ?? workspace.open_sessions ?? 0);
  const chip = (tone, label) => `<span class="tf-chip ${tone}"><span class="tf-chip-dot"></span>${escapeHtml(label)}</span>`;
  const muted = (text) => `<div class="tf-table__cell-sub">${escapeHtml(text)}</div>`;
  const lines = (title, sub) => `<div class="tf-table__cell-title">${escapeHtml(title)}</div>`
    + `<div class="tf-table__cell-sub">${escapeHtml(sub)}</div>`;
  if (status === 'error') return chip('err', t('status_error'));
  if (status === 'provisioning') return chip('info', t('status_provisioning'));
  if (status === 'archived') return muted(t('status_archived'));
  if (open === 0) return muted(t('sessions_none'));
  return lines(t('sessions_count', { count: open }), t('status_active'));
}

// The list handler deliberately reports disk_used_bytes = 0 (walking N repository
// trees per listing is not affordable), so the column shows the limit it does
// know and the usage bar belongs to the workspace detail, which measures it.
function quotaCell(workspace) {
  const quota = quotaBytes(workspace);
  return `<div class="tf-table__cell-title">${escapeHtml(quota ? formatBytes(quota) : '—')}</div>`
    + `<div class="tf-table__cell-sub">${escapeHtml(quota ? t('quota_limit_sub') : t('kpi_disk_no_quota'))}</div>`;
}

// The scheme, the credentials part of an scp-style ssh remote and the `.git`
// suffix are the same on every row — they cost width without separating one
// checkout from another. The full URL stays reachable as the cell's tooltip.
function repoPath(repoUrl) {
  return String(repoUrl)
    .replace(/^[a-z+]+:\/\//i, '')
    .replace(/^[^@/]+@/, '')
    .replace(/:(?=[^\d])/, '/')
    .replace(/\.git$/, '');
}

// A branch belongs to a repository. Printing "main" under "no repository"
// names the branch of a thing that does not exist, so the empty case is one
// muted line and nothing else.
function repoCell(workspace) {
  const repoUrl = String(workspace.repoUrl ?? workspace.repo_url ?? '');
  if (!repoUrl) return `<div class="tf-table__cell-sub">${escapeHtml(t('repo_none'))}</div>`;
  const branch = String(workspace.targetBranch ?? workspace.target_branch
    ?? workspace.defaultBranch ?? workspace.default_branch ?? '');
  // A path and a ref are SCANNED, not read: monospace keeps the segments in
  // fixed columns, which is what makes two similar remotes distinguishable.
  return `<div class="tf-table__cell--mono" title="${escapeAttr(repoUrl)}">`
    + `<div class="tf-table__cell-title">${escapeHtml(repoPath(repoUrl))}</div>`
    + (branch ? `<div class="tf-table__cell-sub">${escapeHtml(branch)}</div>` : '')
    + '</div>';
}

// The caption under the node name only earns its line when it separates rows.
// With a single node in the mesh every row would repeat "this node", which
// says nothing about the record it sits in.
function nodeCell(workspace) {
  const name = String(workspace.nodeName ?? workspace.node_name ?? '');
  const local = !!(workspace.isLocal ?? workspace.is_local);
  const line = `<div class="tf-table__cell-title">${escapeHtml(name)}</div>`;
  if (local && state.nodes.length < 2) return line;
  return line + `<div class="tf-table__cell-sub">${escapeHtml(local ? t('node_local') : t('node_remote'))}</div>`;
}

// Phone card: name, mode badge, and one muted line that answers the two
// questions the dropped columns answered — what is checked out, and is anything
// running. Separator dots only join parts that exist, so nothing reads as a
// missing value.
function summaryCell(workspace) {
  const status = statusOf(workspace);
  const open = Number(workspace.openSessions ?? workspace.open_sessions ?? 0);
  const repoUrl = String(workspace.repoUrl ?? workspace.repo_url ?? '');
  const branch = String(workspace.targetBranch ?? workspace.target_branch
    ?? workspace.defaultBranch ?? workspace.default_branch ?? '');
  const mode = modeChip(workspace);

  const sessions = status === 'error' ? t('status_error')
    : status === 'provisioning' ? t('status_provisioning')
      : status === 'archived' ? t('status_archived')
        : open > 0 ? t('sessions_count', { count: open }) : t('sessions_none');
  const repo = repoUrl
    ? `${repoLabel(repoUrl)}${branch ? ` · ${branch}` : ''}`
    : t('repo_none');
  const parts = [sessions, repo, relativeTimestamp(workspace.updatedAt ?? workspace.updated_at)];

  return `<div class="tf-table__cell-row">`
    + `<span class="tf-table__cell-title">${escapeHtml(String(workspace.name ?? ''))}</span>`
    + `<span class="tf-chip ${mode.status}"><span class="tf-chip-dot"></span>${escapeHtml(mode.label)}</span>`
    + `</div>`
    + `<div class="tf-table__cell-sub">${escapeHtml(parts.join(' · '))}</div>`;
}

// A phone has no room for `https://github.com/org/repo.git`; the owner/name
// pair is the part that identifies the checkout.
function repoLabel(repoUrl) {
  const path = String(repoUrl).replace(/\.git$/, '').split(/[/:]/).filter(Boolean);
  return path.slice(-2).join('/') || repoUrl;
}

function workspaceRow(workspace) {
  const id = wsId(workspace);
  return {
    _id: id,
    summary: summaryCell(workspace),
    workspace: `<div class="tf-table__cell-title">${escapeHtml(String(workspace.name ?? ''))}</div>`
      + `<div class="tf-table__cell-sub">${escapeHtml(shortId(id))}</div>`,
    node: nodeCell(workspace),
    repo: repoCell(workspace),
    mode: modeChip(workspace),
    sessions: sessionCell(workspace),
    quota: quotaCell(workspace),
    activity: relativeTimestamp(workspace.updatedAt ?? workspace.updated_at),
  };
}

// A quota column that reads "—" in every row is 190px of nothing, so it only
// appears once at least one workspace actually carries a limit.
function anyQuota(rows) {
  return rows.some((w) => quotaBytes(w) > 0);
}

function tableColumnsHtml(rows) {
  // On a phone tf-table stacks every column into its own captioned block, so
  // the record collapses into ONE unlabelled summary column instead.
  if (state.card) {
    return '<tf-column key="summary" renderer="html"></tf-column>';
  }
  const wide = !state.narrow;
  const cols = [
    `<tf-column key="workspace" label="${escapeAttr(t('col_workspace'))}" renderer="html"></tf-column>`,
  ];
  if (wide) cols.push(`<tf-column key="node" label="${escapeAttr(t('col_node'))}" renderer="html"></tf-column>`);
  if (wide) cols.push(`<tf-column key="repo" label="${escapeAttr(t('col_repo'))}" renderer="html"></tf-column>`);
  cols.push(`<tf-column key="mode" label="${escapeAttr(t('col_mode'))}" renderer="chip"></tf-column>`);
  cols.push(`<tf-column key="sessions" label="${escapeAttr(t('col_sessions'))}" renderer="html"></tf-column>`);
  if (wide && anyQuota(rows)) cols.push(`<tf-column key="quota" label="${escapeAttr(t('col_quota'))}" renderer="html"></tf-column>`);
  if (wide) cols.push(`<tf-column key="activity" label="${escapeAttr(t('col_activity'))}"></tf-column>`);
  return cols.join('');
}

function buildRowMenu(row) {
  const workspace = state.workspaces.find((w) => wsId(w) === row._id);
  if (!workspace) return null;
  const archived = statusOf(workspace) === 'archived';
  const manage = canManage(workspace);

  // The cell lives inside tf-table's shadow root, so the anchor is positioned
  // through DOM properties — a feature stylesheet never reaches in there.
  const wrap = document.createElement('div');
  wrap.style.position = 'relative';
  wrap.style.display = 'flex';
  wrap.style.justifyContent = 'flex-end';

  const trigger = document.createElement('tf-button');
  trigger.setAttribute('variant', 'ghost');
  // The compact size is a 30px target — fine for a mouse, too small for a thumb.
  if (!state.card) trigger.setAttribute('size', 'sm');
  trigger.setAttribute('aria-label', t('row_actions'));
  trigger.textContent = '⋯';
  wrap.appendChild(trigger);

  const menu = document.createElement('tf-menu');
  menu.setAttribute('placement', 'bottom-end');
  // A dropdown that keeps its own row visible has to hang over the rows below
  // it — no placement avoids that, because the actions column is 40px wide and
  // the panel is not. What it CAN do is claim less of them. Not on the phone:
  // there the compact item box would fall under the 44px touch target.
  if (!state.card) menu.setAttribute('compact', '');

  // No `icon` on these items on purpose: the cell lives in tf-table's shadow
  // root, where <use href="#i-..."> resolves against that tree and finds no
  // sprite. Text-only entries are the honest choice, not blank glyphs.
  const addItem = (action, label, opts = {}) => {
    const item = document.createElement('tf-menu-item');
    item.setAttribute('action', action);
    item.textContent = label;
    if (opts.danger) item.setAttribute('danger', '');
    if (opts.disabled) item.setAttribute('disabled', '');
    if (opts.title) item.title = opts.title;
    menu.appendChild(item);
  };

  addItem('open', t('menu_open_session'));
  addItem('settings', t('menu_settings'));
  // Export needs an archive backend Code Studio does not have — the entry is
  // shown as unavailable rather than as a button that does nothing.
  addItem('export', t('menu_export'), { disabled: true, title: t('menu_export_disabled') });
  addItem(archived ? 'unarchive' : 'archive', archived ? t('menu_unarchive') : t('menu_archive'), {
    disabled: !manage,
  });
  addItem('delete', t('menu_delete'), { danger: true, disabled: !manage });

  wrap.appendChild(menu);

  trigger.addEventListener('click', (e) => {
    e.stopPropagation();
    // Anchoring on the row — not on the ⋯ button — is what keeps the panel off
    // the record it was opened from; a menu that hides its own subject makes
    // the reader guess which workspace they are about to delete.
    menu.anchor = wrap.closest('tr') ?? trigger;
    menu.toggle();
  });
  menu.addEventListener('action', (e) => {
    const action = e.detail?.action;
    if (action === 'open') goto(row._id, null);
    else if (action === 'settings') goto(row._id, null);
    else if (action === 'archive') setArchived(workspace, true);
    else if (action === 'unarchive') setArchived(workspace, false);
    else if (action === 'delete') confirmDelete(workspace);
  });
  return wrap;
}

function renderTable() {
  const host = byId('cs-table-host');
  if (!host) return;
  const rows = filteredWorkspaces();

  if (!rows.length) {
    const noneAtAll = state.workspaces.length === 0;
    // An empty list means either "nothing created yet" or "you may not create" —
    // `can_create` decides which, so the message never contradicts the button.
    const message = noneAtAll
      ? (state.canCreate ? t('empty_body') : t('empty_body_no_grant'))
      : t('empty_filtered_hint');
    host.innerHTML = `
      <tf-empty-state icon="code"
        title="${escapeAttr(noneAtAll ? t('empty_title') : t('empty_filtered'))}"
        message="${escapeAttr(message)}">
        ${noneAtAll ? `<tf-button variant="primary" icon="plus" id="cs-empty-new" ${state.canCreate ? '' : 'disabled'}>${escapeHtml(t('new_workspace'))}</tf-button>` : ''}
      </tf-empty-state>
      ${noneAtAll && !state.canCreate ? `<div class="cs-table-footer cs-centered">${escapeHtml(t('empty_hint_no_grant'))}</div>` : ''}
    `;
    byId('cs-empty-new')?.addEventListener('click', () => { if (state.canCreate) openWizard(); });
    return;
  }

  const existing = host.querySelector('tf-table');
  const signature = `${state.card ? 'card' : state.narrow ? 'narrow' : 'wide'}-${anyQuota(rows) ? 'quota' : 'noquota'}`;
  if (!existing || existing.dataset.signature !== signature) {
    host.innerHTML = `<tf-table id="cs-table" sortable data-signature="${signature}">${tableColumnsHtml(rows)}</tf-table>`;
    const fresh = byId('cs-table');
    fresh.rowActions = buildRowMenu;
    fresh.addEventListener('row-click', (e) => {
      const id = e.detail?.row?._id;
      if (id) goto(id, null);
    });
  }
  byId('cs-table').rows = rows.map(workspaceRow);
}

function renderListFooter() {
  const footer = byId('cs-table-footer');
  if (!footer) return;
  const shown = filteredWorkspaces().length;
  const total = state.workspaces.length;
  const grant = state.canCreate ? t('footer_can_create') : t('footer_no_grant');
  footer.textContent = `${t('footer_summary', { shown, total })} · ${grant}`;

  const sub = byId('cs-list-sub');
  if (sub) {
    const sessions = state.workspaces.reduce(
      (sum, w) => sum + Number(w.openSessions ?? w.open_sessions ?? 0), 0,
    );
    sub.textContent = t('list_sub', { count: total, sessions });
  }
}

function renderList() {
  syncNodeFilter();
  syncFilterChips();
  renderKpis();
  renderTable();
  renderListFooter();
  const newBtn = byId('cs-new');
  if (newBtn) {
    if (state.canCreate) newBtn.removeAttribute('disabled');
    else newBtn.setAttribute('disabled', '');
    newBtn.title = state.canCreate ? '' : t('empty_hint_no_grant');
  }
}

async function refreshList() {
  try {
    await fetchWorkspaces();
  } catch (err) {
    reportError(err);
    return;
  }
  renderList();
}

function wireListEvents() {
  byId('cs-search')?.addEventListener('search', (e) => {
    state.search = String(e.detail?.value ?? '');
    renderTable();
    renderListFooter();
  });
  byId('cs-node-filter')?.addEventListener('change', (e) => {
    state.nodeFilter = String(e.detail?.value ?? 'all');
    renderTable();
    renderListFooter();
  });
  byId('cs-filter')?.addEventListener('change', async (e) => {
    state.listFilter = LIST_FILTERS.includes(e.detail?.id) ? e.detail.id : 'all';
    // The chip is the only control for archived workspaces, so it also decides
    // whether the listing request asks the server for them at all.
    const wantArchived = state.listFilter === 'archived';
    if (wantArchived !== state.includeArchived) {
      state.includeArchived = wantArchived;
      await refreshList();
      return;
    }
    renderTable();
    renderListFooter();
  });
  byId('cs-refresh')?.addEventListener('click', () => refreshList());
  byId('cs-new')?.addEventListener('click', () => { if (state.canCreate) openWizard(); });
}

// =============================================================================
// Workspace actions shared by the list and the detail view
// =============================================================================

async function setArchived(workspace, archived) {
  try {
    await ApiBinary.action('codeStudioWorkspaceArchiveRequest', {
      workspaceId: wsId(workspace),
      archived,
    });
    toast(t(archived ? 'archived_ok' : 'unarchived_ok'), 'success');
    await refreshList();
    if (state.view === 'workspace' && state.workspaceId === wsId(workspace)) {
      await enterWorkspace(state.workspaceId);
    }
  } catch (err) {
    reportError(err);
  }
}

function confirmDelete(workspace) {
  const id = wsId(workspace);
  const name = String(workspace.name ?? '');
  const { body, foot, cleanup } = openWindow({
    title: t('delete_confirm_title'),
    icon: 'trash',
    width: 460,
  });
  body.innerHTML = `
    <div class="cs-warnbox danger">
      ${sprite('alert')}
      <div>${escapeHtml(t('delete_confirm_body', { name }))}</div>
    </div>
    <div class="cs-field cs-mt-14">
      <tf-input id="cs-del-confirm" label="${escapeAttr(t('delete_confirm_input', { name }))}"></tf-input>
    </div>
    <div class="cs-form-error" id="cs-del-error" hidden></div>
  `;
  foot.innerHTML = `
    <tf-button variant="outline" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
    <tf-button variant="danger" data-action="confirm">${escapeHtml(t('action_delete'))}</tf-button>
  `;
  foot.querySelector('[data-action="cancel"]').addEventListener('click', cleanup);
  foot.querySelector('[data-action="confirm"]').addEventListener('click', async () => {
    const typed = String(byId('cs-del-confirm')?.value ?? '').trim();
    const err = byId('cs-del-error');
    if (typed !== name) {
      err.hidden = false;
      err.textContent = t('delete_confirm_mismatch');
      return;
    }
    try {
      await ApiBinary.action('codeStudioWorkspaceDeleteRequest', { workspaceId: id });
      cleanup();
      toast(t('deleted_ok'), 'success');
      await goto(null, null);
    } catch (e) {
      err.hidden = false;
      err.textContent = describeError(e);
    }
  });
}

// =============================================================================
// Window helper (tf-window + backdrop, same contract as Project Studio)
// =============================================================================

function openWindow({ title, icon, width = 640 }) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', title);
  win.setAttribute('icon', icon || 'terminal');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', String(width));
  win.setAttribute('draggable', '');

  const body = document.createElement('div');
  body.slot = 'body';
  win.appendChild(body);

  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.className = 'cs-window-foot';
  win.appendChild(foot);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);

  const cleanup = () => {
    if (win.isConnected) win.close(true);
    if (backdrop.isConnected) backdrop.remove();
    state.wins.delete(cleanup);
  };
  state.wins.add(cleanup);
  win.addEventListener('close-request', () => {
    if (backdrop.isConnected) backdrop.remove();
    state.wins.delete(cleanup);
  });
  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'close') cleanup();
  });
  return { win, body, foot, cleanup };
}

function closeAllWindows() {
  [...state.wins].forEach((fn) => fn());
  state.wins.clear();
}

// =============================================================================
// W02 — creation wizard
// =============================================================================

function nodeById(nodeId) {
  return state.nodes.find((n) => String(n.nodeId ?? n.node_id) === String(nodeId)) ?? null;
}

function openWizard() {
  const firstNode = state.nodes[0];
  const wz = {
    step: 1,
    name: '',
    nodeId: String(firstNode?.nodeId ?? firstNode?.node_id ?? ''),
    execMode: 'trusted_native',
    containerImage: '',
    autonomyCeiling: 'normal',
    egressPolicy: 'org_approved',
    repoKind: 'empty',
    repoUrl: '',
    repoAuthKind: 'none',
    secretMaterial: '',
    defaultBranch: 'main',
    indexEnabled: false,
    busy: false,
  };

  const { win, body, foot, cleanup } = openWindow({ title: t('wizard_title'), icon: 'plus', width: 820 });

  body.innerHTML = `
    <div class="cs-stepper">
      <div class="cs-step" data-pill="1"><span class="n">1</span>${escapeHtml(t('wizard_step1'))}</div>
      <div class="cs-step-line"></div>
      <div class="cs-step" data-pill="2"><span class="n">2</span>${escapeHtml(t('wizard_step2'))}</div>
      <div class="cs-step-line"></div>
      <div class="cs-step" data-pill="3"><span class="n">3</span>${escapeHtml(t('wizard_step3'))}</div>
    </div>

    <div data-panel="1">
      <div class="cs-field">
        <tf-input id="cs-wz-name" label="${escapeAttr(t('name_label'))}" hint="${escapeAttr(t('name_hint'))}"></tf-input>
        <div class="cs-field-hint" id="cs-wz-name-state" hidden></div>
      </div>
      <div class="cs-field">
        <span class="cs-field-label">${escapeHtml(t('node_label'))}</span>
        <tf-select id="cs-wz-node"></tf-select>
        <div class="cs-field-hint">${escapeHtml(t('node_hint'))}</div>
      </div>
      <div class="cs-step-note">
        <b>${escapeHtml(t('dir_note_lead'))}</b>
        ${escapeHtml(t('dir_note_body'))}
        <span class="cs-mono">&lt;data&gt;/code-studio/&lt;id&gt;/</span>
      </div>
    </div>

    <div data-panel="2" hidden>
      <div class="cs-field">
        <span class="cs-field-label">${escapeHtml(t('mode_label'))}</span>
        <tf-choice-group id="cs-wz-modes" columns="2" value="${escapeAttr(wz.execMode)}"
          aria-label="${escapeAttr(t('mode_label'))}">
          <tf-choice-card value="trusted_native" icon="zap"
            heading="${escapeAttr(t('mode_native'))}"
            description="${escapeAttr(t('mode_native_lead'))}"
            pill="${escapeAttr(t('mode_default_tag'))}" pill-tone="warn"></tf-choice-card>
          <tf-choice-card value="container" icon="shield"
            heading="${escapeAttr(t('mode_container'))}"
            description="${escapeAttr(t('mode_container_lead'))}"></tf-choice-card>
        </tf-choice-group>
        <div class="cs-field-hint">${escapeHtml(t('mode_immutable_hint'))}</div>
      </div>
      <div class="cs-field" id="cs-wz-image-field" hidden>
        <tf-input id="cs-wz-image" label="${escapeAttr(t('image_label'))}"
          placeholder="${escapeAttr(t('image_placeholder'))}"
          hint="${escapeAttr(t('image_hint'))}"></tf-input>
      </div>
      <div class="cs-form-row">
        <div class="cs-field">
          <span class="cs-field-label">${escapeHtml(t('autonomy_label'))}</span>
          <tf-select id="cs-wz-autonomy"></tf-select>
          <div class="cs-field-hint warn" id="cs-wz-autonomy-hint" hidden></div>
        </div>
        <div class="cs-field">
          <span class="cs-field-label">${escapeHtml(t('egress_label'))}</span>
          <tf-select id="cs-wz-egress"></tf-select>
          <div class="cs-field-hint warn" id="cs-wz-egress-hint" hidden></div>
        </div>
      </div>
      <div class="cs-step-note" id="cs-wz-native-note" hidden>${escapeHtml(t('native_hidden_note'))}</div>
    </div>

    <div data-panel="3" hidden>
      <div class="cs-field">
        <span class="cs-field-label">${escapeHtml(t('source_label'))}</span>
        <tf-segmented id="cs-wz-source" value="empty">
          <option value="empty" icon="folder">${escapeHtml(t('source_empty'))}</option>
          <option value="git" icon="branch">${escapeHtml(t('source_git'))}</option>
        </tf-segmented>
      </div>
      <div id="cs-wz-git" hidden>
        <div class="cs-field">
          <tf-input id="cs-wz-url" label="${escapeAttr(t('repo_url_label'))}" hint="${escapeAttr(t('repo_url_hint'))}"></tf-input>
        </div>
        <div class="cs-form-row">
          <div class="cs-field">
            <span class="cs-field-label">${escapeHtml(t('auth_label'))}</span>
            <tf-select id="cs-wz-auth" value="none">
              ${AUTH_KINDS.map((k) => `<option value="${k}">${escapeHtml(t(`auth_${k}`))}</option>`).join('')}
            </tf-select>
          </div>
          <div class="cs-field">
            <tf-input id="cs-wz-branch" label="${escapeAttr(t('branch_label'))}" value="main" hint="${escapeAttr(t('branch_hint'))}"></tf-input>
          </div>
        </div>
        <div class="cs-field" id="cs-wz-secret-field" hidden>
          <tf-textarea id="cs-wz-secret" rows="3"
            label="${escapeAttr(t('secret_label'))}"
            hint="${escapeAttr(t('secret_hint'))}"></tf-textarea>
        </div>
        <div class="cs-step-note">${escapeHtml(t('ssh_fingerprint_note'))}</div>
      </div>
      <div class="cs-row cs-mt-12">
        <div>
          <div class="nm">${escapeHtml(t('index_label'))}</div>
          <div class="sub plain">${escapeHtml(t('index_desc'))}</div>
        </div>
        <span class="spacer"></span>
        <tf-toggle id="cs-wz-index"></tf-toggle>
      </div>
    </div>

    <div class="cs-form-error" id="cs-wz-error" hidden></div>
  `;

  foot.innerHTML = `
    <tf-button variant="outline" data-action="back">${escapeHtml(t('action_back'))}</tf-button>
    <tf-button variant="outline" class="cs-foot-push" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
    <tf-button variant="primary" data-action="next"></tf-button>
  `;

  const showError = (message) => {
    const el = byId('cs-wz-error');
    el.hidden = !message;
    el.textContent = message || '';
    // The error sits at the end of a step that can be taller than the window,
    // so on a phone it would otherwise land under the footer, unseen.
    if (message) el.scrollIntoView({ block: 'nearest' });
  };

  // Names are unique per (org, owner) on the server, and the listing already
  // carries every workspace this user owns — so the collision is answered here
  // instead of after a round trip. The server stays the authority.
  const renderNameState = () => {
    const el = byId('cs-wz-name-state');
    const name = wz.name.trim().toLowerCase();
    el.hidden = !name;
    if (!name) return;
    const taken = state.workspaces.some((w) => myRole(w) === 'owner'
      && String(w.name ?? '').trim().toLowerCase() === name);
    el.classList.toggle('ok', !taken);
    el.classList.toggle('warn', taken);
    // Colour alone made the verdict read as one more grey hint, so the state
    // carries its own glyph — the body is light DOM, so the page sprite resolves.
    el.innerHTML = `${sprite(taken ? 'alert' : 'check')}<span>${escapeHtml(taken ? t('name_taken') : t('name_free'))}</span>`;
  };

  // The node decides where the code runs, so the option carries every fact the
  // catalog has about it: whether it is this Core, whether it can isolate a
  // workspace in a container, and how it would enforce egress. `WorkspaceNodeInfo`
  // carries nothing else — operating system and memory are not on the wire.
  const renderNodes = () => {
    byId('cs-wz-node').setOptions(state.nodes.map((n) => {
      const id = String(n.nodeId ?? n.node_id ?? '');
      const supports = !!(n.supportsContainer ?? n.supports_container);
      const enforcement = String(n.egressEnforcement ?? n.egress_enforcement ?? 'unrestricted');
      const facts = [
        (n.isLocal ?? n.is_local) ? t('node_local') : t('node_remote'),
        t(supports ? 'node_container_yes' : 'node_container_no'),
        t(`enforcement_${enforcement}`),
      ];
      return { value: id, label: `${n.name ?? id} — ${facts.join(' · ')}` };
    }), wz.nodeId);
  };

  // The consequence lists never change; only the availability of `container`
  // does, so the cards are filled once and the node switch repaints just that
  // one card. The group owns the selection — clicking a card is intent only.
  const buildModes = () => {
    const group = byId('cs-wz-modes');
    const [native, container] = group.children;
    native.features = [
      { icon: 'check', tone: 'ok', text: t('mode_native_pro1') },
      { icon: 'check', tone: 'ok', text: t('mode_native_pro2') },
      { icon: 'alert', tone: 'warn', lead: t('mode_native_risk1') },
      { icon: 'alert', tone: 'warn', text: t('mode_native_risk2') },
    ];
    container.features = [
      { icon: 'check', tone: 'ok', text: t('mode_container_pro1') },
      { icon: 'check', tone: 'ok', text: t('mode_container_pro2') },
      { icon: 'check', tone: 'ok', text: t('mode_container_pro3') },
      { icon: 'clock', tone: 'muted', text: t('mode_container_slow') },
    ];
    group.addEventListener('change', (e) => {
      wz.execMode = String(e.detail?.value ?? 'trusted_native');
      renderPolicySelects();
    });
  };

  // A node without a container runtime blocks the choice — and the card says
  // what to install, which is the one thing a blocked option must not hide.
  const renderModes = () => {
    const node = nodeById(wz.nodeId);
    const supports = !!(node?.supportsContainer ?? node?.supports_container);
    const group = byId('cs-wz-modes');
    const container = group.querySelector('tf-choice-card[value="container"]');
    container.disabled = !supports;
    container.note = supports ? '' : t('mode_container_unavailable', { node: String(node?.name ?? '') });
    group.value = wz.execMode;
  };

  // In native mode the two unenforceable options are REMOVED from the lists —
  // not disabled — and a one-sentence reason stands under the field. The server
  // rejects them regardless of what the UI shows (§9.5).
  const renderPolicySelects = () => {
    const native = wz.execMode === 'trusted_native';
    const autonomy = byId('cs-wz-autonomy');
    const egress = byId('cs-wz-egress');
    const modes = AUTONOMY_MODES.filter((m) => !(native && m === NATIVE_BLOCKED_AUTONOMY));
    const policies = EGRESS_POLICIES.filter((p) => !(native && p === NATIVE_BLOCKED_EGRESS));
    if (!modes.includes(wz.autonomyCeiling)) wz.autonomyCeiling = 'auto_edit';
    if (!policies.includes(wz.egressPolicy)) wz.egressPolicy = 'org_approved';

    autonomy.setOptions(modes.map((m) => ({ value: m, label: t(`autonomy.${m}`) })), wz.autonomyCeiling);
    egress.setOptions(policies.map((p) => ({ value: p, label: t(`egress_${p}`) })), wz.egressPolicy);

    byId('cs-wz-autonomy-hint').hidden = !native;
    byId('cs-wz-autonomy-hint').textContent = t('autonomy_native_hint');
    byId('cs-wz-egress-hint').hidden = !native;
    byId('cs-wz-egress-hint').textContent = t('egress_native_hint');
    byId('cs-wz-native-note').hidden = !native;
    // The image is the one thing a container workspace cannot be provisioned
    // without (§9.5), so it is asked for exactly where the mode is chosen.
    byId('cs-wz-image-field').hidden = native;
  };

  const paintStep = () => {
    body.querySelectorAll('[data-panel]').forEach((panel) => {
      panel.hidden = Number(panel.dataset.panel) !== wz.step;
    });
    body.querySelectorAll('[data-pill]').forEach((pill) => {
      const n = Number(pill.dataset.pill);
      pill.classList.toggle('active', n === wz.step);
      pill.classList.toggle('done', n < wz.step);
    });
    const back = foot.querySelector('[data-action="back"]');
    back.hidden = wz.step === 1;
    const next = foot.querySelector('[data-action="next"]');
    // `label` is the component's own text channel — writing textContent here
    // would replace the <button> tf-button built with a bare text node.
    next.setAttribute('label', wz.step === 3 ? t('create_btn') : t('action_next'));
    if (wz.step === 3) next.setAttribute('icon', 'check');
    else next.removeAttribute('icon');
    showError('');
    // Each step starts at its own top; otherwise a long step 2 leaves step 3
    // scrolled into its middle.
    win.scrollBodyTop();
  };

  renderNodes();
  buildModes();
  renderModes();
  renderPolicySelects();
  paintStep();

  byId('cs-wz-name').addEventListener('input', (e) => {
    wz.name = String(e.detail?.value ?? '');
    renderNameState();
  });
  byId('cs-wz-node').addEventListener('change', (e) => {
    wz.nodeId = String(e.detail?.value ?? '');
    const node = nodeById(wz.nodeId);
    const supports = !!(node?.supportsContainer ?? node?.supports_container);
    if (!supports && wz.execMode === 'container') wz.execMode = 'trusted_native';
    renderModes();
    renderPolicySelects();
  });
  byId('cs-wz-image').addEventListener('input', (e) => { wz.containerImage = String(e.detail?.value ?? ''); });
  byId('cs-wz-autonomy').addEventListener('change', (e) => { wz.autonomyCeiling = String(e.detail?.value ?? 'normal'); });
  byId('cs-wz-egress').addEventListener('change', (e) => { wz.egressPolicy = String(e.detail?.value ?? 'org_approved'); });
  byId('cs-wz-source').addEventListener('change', (e) => {
    wz.repoKind = e.detail?.value === 'git' ? 'git' : 'empty';
    byId('cs-wz-git').hidden = wz.repoKind !== 'git';
  });
  byId('cs-wz-url').addEventListener('input', (e) => { wz.repoUrl = String(e.detail?.value ?? ''); });
  byId('cs-wz-branch').addEventListener('input', (e) => { wz.defaultBranch = String(e.detail?.value ?? ''); });
  byId('cs-wz-auth').addEventListener('change', (e) => {
    wz.repoAuthKind = String(e.detail?.value ?? 'none');
    byId('cs-wz-secret-field').hidden = wz.repoAuthKind === 'none';
    // An empty box for "private key or token" says nothing about which of the
    // two shapes is expected; the first line of the material does.
    if (wz.repoAuthKind !== 'none') {
      byId('cs-wz-secret').setAttribute('placeholder', t(`secret_placeholder_${wz.repoAuthKind}`));
    }
  });
  byId('cs-wz-secret').addEventListener('input', (e) => { wz.secretMaterial = String(e.detail?.value ?? ''); });
  byId('cs-wz-index').addEventListener('change', (e) => { wz.indexEnabled = !!e.detail?.checked; });

  foot.querySelector('[data-action="cancel"]').addEventListener('click', cleanup);
  foot.querySelector('[data-action="back"]').addEventListener('click', () => {
    if (wz.step > 1) { wz.step -= 1; paintStep(); }
  });
  foot.querySelector('[data-action="next"]').addEventListener('click', async () => {
    if (wz.busy) return;
    if (wz.step === 1) {
      if (!wz.name.trim()) { showError(t('err_name_required')); return; }
      if (!wz.nodeId) { showError(t('err_node_required')); return; }
      wz.step = 2; paintStep(); return;
    }
    if (wz.step === 2) {
      if (wz.execMode === 'container' && !wz.containerImage.trim()) {
        showError(t('err_container_image_required'));
        return;
      }
      wz.step = 3; paintStep(); return;
    }
    if (wz.repoKind === 'git' && !wz.repoUrl.trim()) { showError(t('err_repo_url_required')); return; }

    wz.busy = true;
    try {
      const created = await ApiBinary.action('codeStudioWorkspaceCreateRequest', {
        name: wz.name.trim(),
        nodeId: wz.nodeId,
        execMode: wz.execMode,
        containerImage: wz.execMode === 'container' ? wz.containerImage.trim() : '',
        repoKind: wz.repoKind,
        repoUrl: wz.repoKind === 'git' ? wz.repoUrl.trim() : '',
        repoAuthKind: wz.repoKind === 'git' ? wz.repoAuthKind : 'none',
        secretMaterial: wz.repoKind === 'git' ? wz.secretMaterial : '',
        defaultBranch: wz.defaultBranch.trim(),
        autonomyCeiling: wz.autonomyCeiling,
        egressPolicy: wz.egressPolicy,
        indexEnabled: wz.indexEnabled,
        members: [],
      });
      const id = String(created.workspaceId ?? created.workspace_id ?? '');
      cleanup();
      await refreshList();
      if (id) await goto(id, null);
    } catch (err) {
      showError(describeError(err));
    } finally {
      wz.busy = false;
    }
  });
}

// =============================================================================
// Workspace detail — provisioning, sessions, settings
// =============================================================================

async function enterWorkspace(workspaceId) {
  const host = byId('cs-workspace-view');
  if (!host) return;
  host.innerHTML = `<div class="cs-empty"><tf-spinner></tf-spinner><p>${escapeHtml(t('loading'))}</p></div>`;
  try {
    await fetchWorkspace(workspaceId);
  } catch (err) {
    host.innerHTML = `<div class="cs-form-error">${escapeHtml(describeError(err))}</div>`;
    return;
  }
  if (!state.workspace) {
    host.innerHTML = `<tf-empty-state icon="alert" title="${escapeAttr(t('workspace_missing'))}"></tf-empty-state>`;
    return;
  }
  await Promise.allSettled([
    fetchSessions(workspaceId),
    fetchAllowlist(workspaceId),
    fetchIndexStatus(workspaceId),
  ]);
  renderWorkspaceView();
  if (statusOf(state.workspace) === 'provisioning') startProvisionTracking();
}

function renderWorkspaceView() {
  const host = byId('cs-workspace-view');
  const w = state.workspace;
  if (!host || !w) return;
  const native = isNative(w);
  const enforcement = enforcementOf(w);
  const status = statusOf(w);
  const manage = canManage(w);

  host.innerHTML = `
    <div class="cs-detail-head">
      <tf-button variant="ghost" icon="chevron-left" id="cs-back">${escapeHtml(t('back_to_list'))}</tf-button>
      <div>
        <h1>${escapeHtml(String(w.name ?? ''))}</h1>
        <div class="sub">${escapeHtml(shortId(wsId(w)))} · ${escapeHtml(String(w.nodeName ?? w.node_name ?? ''))}</div>
      </div>
      <span class="cs-chip ${native ? 'warn' : 'ok'}" title="${escapeAttr(native ? t('mode_native_tooltip') : t('mode_container_tooltip'))}">
        ${sprite(native ? 'alert' : 'shield')}${escapeHtml(native ? t('mode_native') : t('mode_container'))}
      </span>
      <span class="cs-chip">${escapeHtml(t(`status_${status}`))}</span>
      <span class="spacer"></span>
      <tf-button variant="primary" icon="play" id="cs-open-session" ${status === 'active' ? '' : 'disabled'}>${escapeHtml(t('session_new'))}</tf-button>
    </div>

    ${native ? `<div class="cs-warnbox">${sprite('alert')}<div><b>${escapeHtml(t('native_banner_title'))}</b> ${escapeHtml(t('native_banner_body'))}</div></div>` : ''}
    ${status === 'error' ? `<div class="cs-warnbox danger cs-mt-10">${sprite('alert')}<div><b>${escapeHtml(t('prov_error_title'))}</b> ${escapeHtml(String(w.statusDetail ?? w.status_detail ?? ''))}</div></div>` : ''}

    <div id="cs-prov-section"></div>

    <div class="cs-section">
      <h3>${sprite('play')}${escapeHtml(t('sessions_title'))}</h3>
      <div class="desc">${escapeHtml(t('sessions_desc'))}</div>
      <div class="cs-row-list" id="cs-session-list"></div>
    </div>

    <div class="cs-section">
      <h3>${sprite('settings')}${escapeHtml(t('set_basics_title'))}</h3>
      <div class="desc">${escapeHtml(t('set_basics_desc'))}</div>

      <div class="cs-readonly-field">
        <div>
          <div class="lbl">${escapeHtml(t('exec_mode_ro_label'))}: ${escapeHtml(native ? t('mode_native') : t('mode_container'))}</div>
          <div class="why">${escapeHtml(t('exec_mode_ro_why'))}</div>
        </div>
        <span class="spacer"></span>
        <span class="cs-chip ${native ? 'warn' : 'ok'}">${escapeHtml(t('field_readonly'))}</span>
      </div>

      <div class="cs-readonly-field cs-mt-8">
        <div>
          <div class="lbl">${escapeHtml(t('enforcement_label'))}: ${escapeHtml(t(`enforcement_${enforcement}`))}</div>
          <div class="why">${escapeHtml(t(`enforcement_${enforcement}_why`))}</div>
        </div>
      </div>
      ${enforcement === 'unrestricted' ? `<div class="cs-warnbox cs-mt-8">${sprite('alert')}<div>${escapeHtml(t('enforcement_unrestricted_warn'))}</div></div>` : ''}

      <div class="cs-field cs-mt-14">
        <tf-input id="cs-set-name" label="${escapeAttr(t('set_name'))}" value="${escapeAttr(String(w.name ?? ''))}" ${manage ? '' : 'readonly'}></tf-input>
      </div>
      <div class="cs-form-row">
        <div class="cs-field">
          <tf-input id="cs-set-branch" label="${escapeAttr(t('set_target_branch'))}"
            value="${escapeAttr(String(w.targetBranch ?? w.target_branch ?? ''))}"
            hint="${escapeAttr(t('set_target_branch_hint'))}" ${manage ? '' : 'readonly'}></tf-input>
        </div>
        <div class="cs-field">
          <span class="cs-field-label">${escapeHtml(t('set_autonomy_ceiling'))}</span>
          <tf-select id="cs-set-autonomy" ${manage ? '' : 'disabled'}></tf-select>
        </div>
      </div>
      <div class="cs-form-row">
        <div class="cs-field">
          <span class="cs-field-label">${escapeHtml(t('set_egress_policy'))}</span>
          <tf-select id="cs-set-egress" ${manage ? '' : 'disabled'}></tf-select>
        </div>
        <div class="cs-field">
          <tf-input id="cs-set-quota-disk" type="number" label="${escapeAttr(t('set_quota_disk'))}"
            value="${quotaBytes(w) ? String(Math.round(quotaBytes(w) / (1024 * 1024 * 1024))) : ''}"
            hint="${escapeAttr(t('set_quota_disk_hint'))}" ${manage ? '' : 'readonly'}></tf-input>
        </div>
      </div>
      <div class="cs-form-row">
        <div class="cs-field">
          <tf-input id="cs-set-quota-sessions" type="number" label="${escapeAttr(t('set_quota_sessions'))}"
            hint="${escapeAttr(t('set_quota_sessions_hint'))}" ${manage ? '' : 'readonly'}></tf-input>
        </div>
        <div class="cs-row">
          <div>
            <div class="nm">${escapeHtml(t('index_label'))}</div>
            <div class="sub plain">${escapeHtml(t('index_desc'))}</div>
          </div>
          <span class="spacer"></span>
          <tf-toggle id="cs-set-index" ${(w.indexEnabled ?? w.index_enabled) ? 'checked' : ''} ${manage ? '' : 'disabled'}></tf-toggle>
        </div>
      </div>
      <div class="cs-actions-right">
        <tf-button variant="primary" icon="check" id="cs-set-save" ${manage ? '' : 'disabled'}>${escapeHtml(t('action_save'))}</tf-button>
      </div>
      <div class="cs-form-error" id="cs-set-error" hidden></div>
    </div>

    <div class="cs-section">
      <h3>${sprite('key')}${escapeHtml(t('creds_title'))}</h3>
      <div class="desc">${escapeHtml(t('creds_desc'))}</div>
      <div class="cs-readonly-field">
        <div>
          <div class="lbl">${escapeHtml((w.hasSecret ?? w.has_secret) ? t('creds_has_secret') : t('creds_no_secret'))}</div>
          <div class="why">${escapeHtml(String(w.repoUrl ?? w.repo_url ?? t('repo_none')))}</div>
        </div>
      </div>
      <div class="cs-form-row cs-mt-12">
        <div class="cs-field">
          <span class="cs-field-label">${escapeHtml(t('auth_label'))}</span>
          <tf-select id="cs-cred-kind" ${manage ? '' : 'disabled'}></tf-select>
        </div>
        <div class="cs-field">
          <tf-input id="cs-cred-fingerprint" label="${escapeAttr(t('creds_fingerprint'))}"
            hint="${escapeAttr(t('creds_fingerprint_hint'))}" ${manage ? '' : 'readonly'}></tf-input>
        </div>
      </div>
      <div class="cs-field">
        <tf-textarea id="cs-cred-material" rows="3" label="${escapeAttr(t('secret_label'))}"
          hint="${escapeAttr(t('secret_hint'))}" ${manage ? '' : 'disabled'}></tf-textarea>
      </div>
      <div class="cs-actions-right">
        <tf-button variant="primary" icon="key" id="cs-cred-save" ${manage ? '' : 'disabled'}>${escapeHtml(t('creds_save'))}</tf-button>
      </div>
    </div>

    <div class="cs-section">
      <h3>${sprite('users')}${escapeHtml(t('members_title'))}</h3>
      <div class="desc">${escapeHtml(t('members_desc'))}</div>
      <div class="cs-row-list" id="cs-member-list"></div>
      ${manage ? `
        <div class="tf-toolbar cs-mt-12">
          <tf-searchbox id="cs-member-search" placeholder="${escapeAttr(t('member_search'))}" debounce="250"></tf-searchbox>
          <tf-select id="cs-member-role" value="editor">
            ${MEMBER_ROLES.filter((r) => r !== 'owner').map((r) => `<option value="${r}">${escapeHtml(t(`role_${r}`))}</option>`).join('')}
          </tf-select>
        </div>
        <div class="cs-candidates" id="cs-member-candidates" hidden></div>
      ` : ''}
    </div>

    <div class="cs-section">
      <h3>${sprite('shield')}${escapeHtml(t('allow_title'))}</h3>
      <div class="desc">${escapeHtml(t('allow_desc'))}</div>
      ${enforcement === 'unrestricted' ? `<div class="cs-warnbox cs-mb-10">${sprite('alert')}<div>${escapeHtml(t('allow_unrestricted_note'))}</div></div>` : ''}
      <div class="cs-row-list" id="cs-allow-list"></div>
      ${manage ? `
        <div class="tf-toolbar cs-mt-12">
          <tf-select id="cs-allow-cap" value="exec">
            ${ALLOWLIST_CAPABILITIES.map((c) => `<option value="${c}">${escapeHtml(t(`allow_cap_${c}`))}</option>`).join('')}
          </tf-select>
          <tf-input id="cs-allow-pattern" placeholder="${escapeAttr(t('allow_pattern'))}"></tf-input>
          <span class="tf-toolbar-spacer"></span>
          <tf-button variant="ghost" icon="plus" id="cs-allow-add">${escapeHtml(t('action_add'))}</tf-button>
        </div>
      ` : ''}
    </div>

    <div class="cs-section">
      <h3>${sprite('database')}${escapeHtml(t('index_title'))}</h3>
      <div class="desc">${escapeHtml(t('index_section_desc'))}</div>
      <div class="cs-row-list" id="cs-index-list"></div>
    </div>

    <div class="cs-section danger">
      <h3>${sprite('alert')}${escapeHtml(t('danger_title'))}</h3>
      <div class="desc">${escapeHtml(t('danger_desc'))}</div>
      <div class="cs-row">
        <div>
          <div class="nm">${escapeHtml(status === 'archived' ? t('menu_unarchive') : t('menu_archive'))}</div>
          <div class="sub plain">${escapeHtml(t('danger_archive_desc'))}</div>
        </div>
        <span class="spacer"></span>
        <tf-button variant="ghost" id="cs-archive" ${manage ? '' : 'disabled'}>${escapeHtml(status === 'archived' ? t('menu_unarchive') : t('menu_archive'))}</tf-button>
      </div>
      <div class="cs-row cs-mt-8">
        <div>
          <div class="nm">${escapeHtml(t('menu_delete'))}</div>
          <div class="sub plain">${escapeHtml(t('danger_delete_desc'))}</div>
        </div>
        <span class="spacer"></span>
        <tf-button variant="danger" icon="trash" id="cs-delete" ${manage ? '' : 'disabled'}>${escapeHtml(t('action_delete'))}</tf-button>
      </div>
    </div>
  `;

  fillSettingsSelects(w, native);
  renderProvisioning();
  renderSessions();
  renderMembers();
  renderAllowlist();
  renderIndexStatus();
  wireWorkspaceEvents(w);
}

function fillSettingsSelects(w, native) {
  const autonomy = byId('cs-set-autonomy');
  const egress = byId('cs-set-egress');
  const kind = byId('cs-cred-kind');
  if (autonomy) {
    const modes = AUTONOMY_MODES.filter((m) => !(native && m === NATIVE_BLOCKED_AUTONOMY));
    autonomy.setOptions(
      modes.map((m) => ({ value: m, label: t(`autonomy.${m}`) })),
      String(w.autonomyCeiling ?? w.autonomy_ceiling ?? 'normal'),
    );
  }
  if (egress) {
    const policies = EGRESS_POLICIES.filter((p) => !(native && p === NATIVE_BLOCKED_EGRESS));
    egress.setOptions(
      policies.map((p) => ({ value: p, label: t(`egress_${p}`) })),
      String(w.egressPolicy ?? w.egress_policy ?? 'org_approved'),
    );
  }
  if (kind) {
    kind.setOptions(
      AUTH_KINDS.map((k) => ({ value: k, label: t(`auth_${k}`) })),
      String(w.repoAuthKind ?? w.repo_auth_kind ?? 'none'),
    );
  }
}

// ---- Provisioning saga ------------------------------------------------------

function renderProvisioning() {
  const host = byId('cs-prov-section');
  if (!host) return;
  const w = state.workspace;
  const status = statusOf(w);
  if (status !== 'provisioning' && status !== 'error') {
    host.innerHTML = '';
    return;
  }
  const steps = state.provisioning.map((step) => `
    <div class="cs-prov-step ${escapeAttr(String(step.status ?? ''))}">
      <span class="cs-dot ${step.status === 'done' ? 'ok' : (step.status === 'failed' ? 'err' : (status === 'provisioning' ? 'run' : 'idle'))}"></span>
      <span class="nm">${escapeHtml(String(step.step ?? ''))}</span>
      <span class="detail">${escapeHtml(String(step.detail ?? t(`prov_status_${step.status ?? 'pending'}`)))}</span>
    </div>
  `).join('');

  host.innerHTML = `
    <div class="cs-section">
      <h3>${sprite('clock')}${escapeHtml(t('prov_title'))}</h3>
      <div class="desc">${escapeHtml(t('prov_desc'))}</div>
      <div class="cs-prov-list">${steps || `<div class="cs-field-hint">${escapeHtml(t('prov_no_steps'))}</div>`}</div>
      ${status === 'error' ? `<div class="cs-actions-right"><tf-button variant="primary" icon="refresh" id="cs-prov-retry">${escapeHtml(t('prov_retry'))}</tf-button></div>` : ''}
    </div>
  `;
  byId('cs-prov-retry')?.addEventListener('click', async () => {
    try {
      await ApiBinary.action('codeStudioWorkspaceRetryRequest', { workspaceId: state.workspaceId });
      await fetchWorkspace(state.workspaceId);
      renderProvisioning();
      startProvisionTracking();
    } catch (err) {
      reportError(err);
    }
  });
}

// The workspace starts `provisioning`; we FOLLOW it with WorkspaceGetRequest
// instead of assuming success. Polling stops the moment the saga leaves that
// state, in either direction.
function startProvisionTracking() {
  stopProvisionTracking();
  state.provisionTimer = window.setInterval(async () => {
    if (!state.workspaceId || state.view !== 'workspace') {
      stopProvisionTracking();
      return;
    }
    try {
      const w = await fetchWorkspace(state.workspaceId);
      const status = statusOf(w);
      renderProvisioning();
      if (status !== 'provisioning') {
        stopProvisionTracking();
        renderWorkspaceView();
        if (status === 'active') toast(t('prov_done'), 'success');
      }
    } catch {
      stopProvisionTracking();
    }
  }, 2500);
}

function stopProvisionTracking() {
  if (state.provisionTimer) {
    window.clearInterval(state.provisionTimer);
    state.provisionTimer = null;
  }
}

// ---- Sessions ---------------------------------------------------------------

function renderSessions() {
  const host = byId('cs-session-list');
  if (!host) return;
  if (!state.sessions.length) {
    host.innerHTML = `<div class="cs-field-hint">${escapeHtml(t('session_none'))}</div>`;
    return;
  }
  host.innerHTML = state.sessions.map((s) => {
    const id = String(s.sessionId ?? s.session_id ?? '');
    const open = String(s.status ?? '') !== 'closed';
    return `
      <div class="cs-row" data-session="${escapeAttr(id)}">
        <span class="cs-dot ${open ? 'run' : 'idle'}"></span>
        <div>
          <div class="nm">${escapeHtml(String(s.title ?? t('session_untitled')))}</div>
          <div class="sub">${escapeHtml(String(s.branch ?? ''))} · ${escapeHtml(t(`autonomy.${s.autonomyMode ?? s.autonomy_mode ?? 'normal'}`))}</div>
        </div>
        <span class="spacer"></span>
        <span class="cs-chip">${escapeHtml(formatTimestamp(s.updatedAt ?? s.updated_at))}</span>
        <tf-button variant="ghost" size="sm" data-open-session="${escapeAttr(id)}">${escapeHtml(t('session_open'))}</tf-button>
        ${open ? `<tf-button variant="ghost" size="sm" data-close-session="${escapeAttr(id)}">${escapeHtml(t('session_close'))}</tf-button>` : ''}
      </div>
    `;
  }).join('');

  host.querySelectorAll('[data-open-session]').forEach((btn) => {
    btn.addEventListener('click', () => goto(state.workspaceId, btn.dataset.openSession));
  });
  host.querySelectorAll('[data-close-session]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      try {
        await ApiBinary.action('codeStudioSessionCloseRequest', {
          workspaceId: state.workspaceId,
          sessionId: btn.dataset.closeSession,
        });
        await fetchSessions(state.workspaceId);
        renderSessions();
      } catch (err) {
        reportError(err);
      }
    });
  });
}

function openSessionDialog() {
  const w = state.workspace;
  if (!w) return;
  const ceiling = String(w.autonomyCeiling ?? w.autonomy_ceiling ?? 'normal');
  const allowed = AUTONOMY_MODES.slice(0, AUTONOMY_MODES.indexOf(ceiling) + 1);
  const { body, foot, cleanup } = openWindow({ title: t('session_new'), icon: 'play', width: 480 });
  body.innerHTML = `
    <div class="cs-field">
      <tf-input id="cs-sess-title" label="${escapeAttr(t('session_title_label'))}" hint="${escapeAttr(t('session_title_hint'))}"></tf-input>
    </div>
    <div class="cs-field">
      <span class="cs-field-label">${escapeHtml(t('session_autonomy_label'))}</span>
      <tf-select id="cs-sess-autonomy" value="${escapeAttr(allowed[allowed.length - 1] ?? 'normal')}">
        ${allowed.map((m) => `<option value="${m}">${escapeHtml(t(`autonomy.${m}`))}</option>`).join('')}
      </tf-select>
      <div class="cs-field-hint">${escapeHtml(t('session_autonomy_hint', { ceiling: t(`autonomy.${ceiling}`) }))}</div>
    </div>
    <div class="cs-step-note">${escapeHtml(t('session_branch_note'))}</div>
    <div class="cs-form-error" id="cs-sess-error" hidden></div>
  `;
  foot.innerHTML = `
    <tf-button variant="outline" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
    <tf-button variant="primary" data-action="create">${escapeHtml(t('session_create'))}</tf-button>
  `;
  foot.querySelector('[data-action="cancel"]').addEventListener('click', cleanup);
  foot.querySelector('[data-action="create"]').addEventListener('click', async () => {
    const title = String(byId('cs-sess-title')?.value ?? '').trim();
    const err = byId('cs-sess-error');
    if (!title) {
      err.hidden = false;
      err.textContent = t('err_session_title_required');
      return;
    }
    try {
      const resp = await ApiBinary.action('codeStudioSessionOpenRequest', {
        workspaceId: state.workspaceId,
        title,
        autonomyMode: String(byId('cs-sess-autonomy')?.value ?? 'normal'),
      });
      const session = resp.session ?? {};
      cleanup();
      await goto(state.workspaceId, String(session.sessionId ?? session.session_id ?? ''));
    } catch (e) {
      err.hidden = false;
      err.textContent = describeError(e);
    }
  });
}

// ---- Members ----------------------------------------------------------------

function renderMembers() {
  const host = byId('cs-member-list');
  if (!host) return;
  const manage = canManage(state.workspace);
  if (!state.members.length) {
    host.innerHTML = `<div class="cs-field-hint">${escapeHtml(t('members_empty'))}</div>`;
    return;
  }
  host.innerHTML = state.members.map((m) => {
    const userId = String(m.userId ?? m.user_id ?? '');
    const role = String(m.role ?? 'viewer');
    return `
      <div class="cs-row">
        <div>
          <div class="nm">${escapeHtml(String(m.displayName ?? m.display_name ?? userId))}</div>
          <div class="sub">${escapeHtml(shortId(userId))} · ${escapeHtml(formatTimestamp(m.addedAt ?? m.added_at))}</div>
        </div>
        <span class="spacer"></span>
        <tf-select data-member-role="${escapeAttr(userId)}" value="${escapeAttr(role)}" ${manage && role !== 'owner' ? '' : 'disabled'}>
          ${MEMBER_ROLES.map((r) => `<option value="${r}">${escapeHtml(t(`role_${r}`))}</option>`).join('')}
        </tf-select>
        ${manage && role !== 'owner' ? `<tf-button variant="ghost" size="sm" data-member-remove="${escapeAttr(userId)}">${escapeHtml(t('member_remove'))}</tf-button>` : ''}
      </div>
    `;
  }).join('');

  host.querySelectorAll('[data-member-role]').forEach((select) => {
    select.addEventListener('change', (e) => setMemberRole(select.dataset.memberRole, String(e.detail?.value ?? 'viewer')));
  });
  host.querySelectorAll('[data-member-remove]').forEach((btn) => {
    btn.addEventListener('click', () => removeMember(btn.dataset.memberRemove));
  });
}

async function setMemberRole(userId, role) {
  try {
    const resp = await ApiBinary.action('codeStudioWorkspaceMemberSetRequest', {
      workspaceId: state.workspaceId, userId, role,
    });
    state.members = Array.isArray(resp.members) ? resp.members : state.members;
    renderMembers();
    toast(t('member_saved'), 'success');
  } catch (err) {
    reportError(err);
  }
}

async function removeMember(userId) {
  try {
    const resp = await ApiBinary.action('codeStudioWorkspaceMemberRemoveRequest', {
      workspaceId: state.workspaceId, userId,
    });
    state.members = Array.isArray(resp.members) ? resp.members : state.members;
    renderMembers();
  } catch (err) {
    reportError(err);
  }
}

// ---- Allowlist --------------------------------------------------------------

function renderAllowlist() {
  const host = byId('cs-allow-list');
  if (!host) return;
  if (!state.allowlist.length) {
    host.innerHTML = `<div class="cs-field-hint">${escapeHtml(t('allow_empty'))}</div>`;
    return;
  }
  const manage = canManage(state.workspace);
  host.innerHTML = state.allowlist.map((entry) => {
    const capability = String(entry.capability ?? '');
    const pattern = String(entry.pattern ?? '');
    return `
      <div class="cs-row">
        <div>
          <div class="nm">${escapeHtml(capability)}</div>
          <div class="sub">${escapeHtml(pattern)}</div>
        </div>
        <span class="spacer"></span>
        ${manage ? `<tf-button variant="ghost" size="sm" data-allow-remove="${escapeAttr(capability)}" data-allow-pattern="${escapeAttr(pattern)}">${escapeHtml(t('action_remove'))}</tf-button>` : ''}
      </div>
    `;
  }).join('');
  host.querySelectorAll('[data-allow-remove]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      try {
        await ApiBinary.action('codeStudioWorkspaceAllowlistRemoveRequest', {
          workspaceId: state.workspaceId,
          capability: btn.dataset.allowRemove,
          pattern: btn.dataset.allowPattern,
        });
        await fetchAllowlist(state.workspaceId);
        renderAllowlist();
      } catch (err) {
        reportError(err);
      }
    });
  });
}

// ---- Semantic index ---------------------------------------------------------

function renderIndexStatus() {
  const host = byId('cs-index-list');
  if (!host) return;
  const w = state.workspace;
  const enabled = !!(w?.indexEnabled ?? w?.index_enabled);
  const branches = Array.isArray(state.indexStatus?.branches) ? state.indexStatus.branches : [];
  if (!enabled) {
    host.innerHTML = `<div class="cs-field-hint">${escapeHtml(t('index_disabled'))}</div>`;
    return;
  }
  if (!branches.length) {
    host.innerHTML = `
      <div class="cs-field-hint">${escapeHtml(t('index_no_branches'))}</div>
      <div class="cs-actions-right">
        <tf-button variant="ghost" icon="refresh" data-index-rebuild="">${escapeHtml(t('index_rebuild'))}</tf-button>
      </div>
    `;
  } else {
    host.innerHTML = branches.map((b) => {
      const branch = String(b.branch ?? '');
      return `
        <div class="cs-row">
          <div>
            <div class="nm">${escapeHtml(branch)}</div>
            <div class="sub">${escapeHtml(String(b.state ?? b.status ?? ''))} · ${escapeHtml(formatTimestamp(b.updatedAt ?? b.updated_at))}</div>
          </div>
          <span class="spacer"></span>
          <tf-button variant="ghost" size="sm" data-index-rebuild="${escapeAttr(branch)}">${escapeHtml(t('index_rebuild'))}</tf-button>
        </div>
      `;
    }).join('');
  }
  host.querySelectorAll('[data-index-rebuild]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      try {
        await ApiBinary.action('codeStudioIndexRebuildRequest', {
          workspaceId: state.workspaceId,
          branch: btn.dataset.indexRebuild,
        });
        toast(t('index_rebuild_started'), 'success');
        await fetchIndexStatus(state.workspaceId);
        renderIndexStatus();
      } catch (err) {
        reportError(err);
      }
    });
  });
}

// ---- Wiring the workspace view ---------------------------------------------

function wireWorkspaceEvents(w) {
  byId('cs-back')?.addEventListener('click', () => goto(null, null));
  byId('cs-open-session')?.addEventListener('click', () => openSessionDialog());
  byId('cs-archive')?.addEventListener('click', () => setArchived(w, statusOf(w) !== 'archived'));
  byId('cs-delete')?.addEventListener('click', () => confirmDelete(w));

  byId('cs-set-save')?.addEventListener('click', async () => {
    const err = byId('cs-set-error');
    err.hidden = true;
    const diskGiB = String(byId('cs-set-quota-disk')?.value ?? '').trim();
    const sessions = String(byId('cs-set-quota-sessions')?.value ?? '').trim();
    try {
      const resp = await ApiBinary.action('codeStudioWorkspaceSettingsUpdateRequest', {
        workspaceId: state.workspaceId,
        name: String(byId('cs-set-name')?.value ?? '').trim(),
        autonomyCeiling: String(byId('cs-set-autonomy')?.value ?? 'normal'),
        egressPolicy: String(byId('cs-set-egress')?.value ?? 'org_approved'),
        targetBranch: String(byId('cs-set-branch')?.value ?? '').trim(),
        indexEnabled: !!byId('cs-set-index')?.checked,
        quotaDiskBytes: diskGiB ? Number(diskGiB) * 1024 * 1024 * 1024 : '',
        quotaSessions: sessions || '',
      });
      state.workspace = resp.workspace ?? state.workspace;
      toast(t('saved_ok'), 'success');
      renderWorkspaceView();
      await refreshList();
    } catch (e) {
      err.hidden = false;
      err.textContent = describeError(e);
    }
  });

  byId('cs-cred-save')?.addEventListener('click', async () => {
    try {
      const resp = await ApiBinary.action('codeStudioWorkspaceSecretSetRequest', {
        workspaceId: state.workspaceId,
        repoAuthKind: String(byId('cs-cred-kind')?.value ?? 'none'),
        secretMaterial: String(byId('cs-cred-material')?.value ?? ''),
        sshHostFingerprint: String(byId('cs-cred-fingerprint')?.value ?? ''),
      });
      const material = byId('cs-cred-material');
      if (material) material.value = '';
      const fingerprint = String(resp.fingerprint ?? '');
      toast(fingerprint ? t('creds_saved_fingerprint', { fingerprint }) : t('creds_saved'), 'success');
      await fetchWorkspace(state.workspaceId);
      renderWorkspaceView();
    } catch (err) {
      reportError(err);
    }
  });

  byId('cs-allow-add')?.addEventListener('click', async () => {
    const pattern = String(byId('cs-allow-pattern')?.value ?? '').trim();
    if (!pattern) return;
    try {
      await ApiBinary.action('codeStudioWorkspaceAllowlistSetRequest', {
        workspaceId: state.workspaceId,
        capability: String(byId('cs-allow-cap')?.value ?? 'exec'),
        pattern,
      });
      byId('cs-allow-pattern').value = '';
      await fetchAllowlist(state.workspaceId);
      renderAllowlist();
    } catch (err) {
      reportError(err);
    }
  });

  byId('cs-member-search')?.addEventListener('search', async (e) => {
    const host = byId('cs-member-candidates');
    const query = String(e.detail?.value ?? '').trim();
    if (!host) return;
    if (!query) {
      host.hidden = true;
      host.innerHTML = '';
      return;
    }
    let users = [];
    try {
      // Code Studio has no member-candidate request of its own; this is the
      // org identity lookup already used by the Project Studio wizard and it is
      // authorized server-side per caller.
      const resp = await ApiBinary.one('projectStudioMemberCandidatesRequest', { query, limit: 12 });
      users = Array.isArray(resp.users) ? resp.users : [];
    } catch (err) {
      host.hidden = false;
      host.innerHTML = `<div class="cs-field-hint">${escapeHtml(describeError(err))}</div>`;
      return;
    }
    const known = new Set(state.members.map((m) => String(m.userId ?? m.user_id)));
    const rows = users.filter((u) => !known.has(String(u.userId ?? u.user_id)));
    host.hidden = false;
    if (!rows.length) {
      host.innerHTML = `<div class="cs-field-hint">${escapeHtml(t('member_no_candidates'))}</div>`;
      return;
    }
    host.replaceChildren(...rows.map((u) => {
      const userId = String(u.userId ?? u.user_id ?? '');
      const row = document.createElement('tf-option-row');
      row.className = 'cs-candidate';
      row.value = userId;
      row.label = String(u.displayName ?? u.display_name ?? userId);
      row.sub = String(u.email ?? shortId(userId));
      row.addEventListener('option-select', async (e) => {
        await setMemberRole(e.detail.value, String(byId('cs-member-role')?.value ?? 'editor'));
        host.hidden = true;
        host.replaceChildren();
      });
      return row;
    }));
  });
}

// =============================================================================
// Session shell — the four state attributes, the workspace bar and the sheet
// =============================================================================

// The session surface builds the stage and the dock straight into `.cs-body`;
// the phone chrome it appends to `.cs-shell` itself. The note lives here and
// not inside the markup: a backtick in an HTML comment CLOSES the template
// literal around it, and the shell then throws instead of rendering.
function sessionShellHtml() {
  return `
    <div class="cs-shell" id="cs-shell" data-stage="konsola" data-dock="agenci" data-view="konsola" data-ask="0">
      <div class="cs-projects" id="cs-projects"></div>
      <div class="cs-mtop" id="cs-mtop">
        <tf-button class="cs-wsbtn" id="cs-wsbtn" variant="ghost" trailing-icon="chevron-down">
          <span class="cs-dot idle"></span>
          <span class="nm"></span>
        </tf-button>
        <span class="ttl">
          <span class="t1"></span>
          <span class="t2"></span>
        </span>
        <tf-button variant="ghost" size="sm" id="cs-mtop-exit">${escapeHtml(t('session_exit'))}</tf-button>
      </div>
      <div class="cs-body" id="cs-session-host"></div>
      <div class="cs-sheet" id="cs-sheet">
        <div class="cs-dock-title">${escapeHtml(t('sheet_title'))}</div>
        <div id="cs-sheet-items"></div>
        <tf-button class="cs-proj-add cs-sheet-new" id="cs-sheet-new" variant="outline"
          size="sm" icon="plus">${escapeHtml(t('new_workspace'))}</tf-button>
        <div class="cs-sheet-grab"></div>
      </div>
      <div class="cs-scrim" id="cs-scrim"></div>
    </div>
  `;
}

// The status dot is this module's own dictionary (`.cs-dot` + a state class),
// so the row component takes it as an element instead of learning the states.
function workspaceDot(workspace) {
  const dot = document.createElement('span');
  dot.className = `cs-dot ${workspaceDotClass(workspace)}`;
  return dot;
}

function workspaceDotClass(workspace) {
  const status = statusOf(workspace);
  if (status === 'error') return 'err';
  if (status === 'provisioning') return 'run';
  if (status === 'archived') return 'idle';
  return Number(workspace.openSessions ?? workspace.open_sessions ?? 0) > 0 ? 'run' : 'idle';
}

function workspaceMetaText(workspace) {
  const node = String(workspace.nodeName ?? workspace.node_name ?? '');
  const open = Number(workspace.openSessions ?? workspace.open_sessions ?? 0);
  const status = statusOf(workspace);
  if (status === 'error') return `${node} · ${t('status_error')}`;
  if (status === 'provisioning') return `${node} · ${t('status_provisioning')}`;
  if (open > 0) return `${node} · ${t('sessions_count', { count: open })}`;
  return `${node} · ${t('sessions_none')}`;
}

/** Patches the workspace bar and the phone sheet in place — never a full redraw. */
function renderWorkspaceBar() {
  const bar = byId('cs-projects');
  const sheet = byId('cs-sheet-items');
  const active = state.workspaceId;
  const rows = state.workspaces.filter((w) => statusOf(w) !== 'archived');

  const optionRow = (w, className) => {
    const row = document.createElement('tf-option-row');
    row.className = className;
    row.value = wsId(w);
    row.label = String(w.name ?? '');
    row.sub = workspaceMetaText(w);
    row.selected = wsId(w) === active;
    row.lead = workspaceDot(w);
    return row;
  };

  if (bar) {
    const addBtn = document.createElement('tf-button');
    addBtn.className = 'cs-proj-add';
    addBtn.setAttribute('variant', 'outline');
    addBtn.setAttribute('size', 'sm');
    addBtn.setAttribute('icon', 'plus');
    addBtn.setAttribute('label', t('new_workspace'));
    addBtn.addEventListener('click', () => { if (state.canCreate) openWizard(); });
    bar.replaceChildren(...rows.map((w) => {
      const row = optionRow(w, 'cs-proj');
      row.addEventListener('option-select', (e) => goto(e.detail.value, null));
      return row;
    }), addBtn);
  }

  if (sheet) {
    sheet.replaceChildren(...rows.map((w) => {
      const row = optionRow(w, 'cs-sheet-item');
      row.addEventListener('option-select', (e) => {
        closeSheet();
        goto(e.detail.value, null);
      });
      return row;
    }));
  }

  const wsbtn = byId('cs-wsbtn');
  const current = state.workspaces.find((w) => wsId(w) === active);
  if (wsbtn && current) {
    wsbtn.querySelector('.cs-dot').className = `cs-dot ${workspaceDotClass(current)}`;
    wsbtn.querySelector('.nm').textContent = String(current.name ?? '');
  }
}

function openSheet() {
  byId('cs-sheet')?.classList.add('open');
  byId('cs-scrim')?.classList.add('open');
}

function closeSheet() {
  byId('cs-sheet')?.classList.remove('open');
  byId('cs-scrim')?.classList.remove('open');
}

function wireShellEvents() {
  byId('cs-wsbtn')?.addEventListener('click', () => {
    const sheet = byId('cs-sheet');
    if (sheet?.classList.contains('open')) closeSheet();
    else openSheet();
  });
  byId('cs-scrim')?.addEventListener('click', closeSheet);
  byId('cs-sheet-new')?.addEventListener('click', () => {
    closeSheet();
    if (state.canCreate) openWizard();
  });
  byId('cs-mtop-exit')?.addEventListener('click', () => goto(state.workspaceId, null));
}

async function loadSessionModule() {
  if (state.sessionModule) return state.sessionModule;
  // Loaded on demand: the registry screens must not pay for the console, and a
  // load failure has to surface as an error instead of taking the module down.
  state.sessionModule = await import('/js/modules/code-studio-session.js');
  return state.sessionModule;
}

async function unmountSessionSurface() {
  // The connectivity watcher goes first: it feeds the console, so it must stop
  // before the console it feeds disappears.
  detachConnection();
  if (!state.sessionModule) return;
  try {
    state.sessionModule.unmountSession();
  } catch (err) {
    console.error('[code-studio] unmountSession failed', err);
  }
}

async function enterSession(workspaceId, sessionId) {
  const view = byId('cs-session-view');
  if (!view) return;
  if (!view.querySelector('.cs-shell')) {
    view.innerHTML = sessionShellHtml();
    wireShellEvents();
  }

  try {
    if (!state.workspace || wsId(state.workspace) !== workspaceId) {
      await fetchWorkspace(workspaceId);
    }
    if (!state.workspaces.length) await fetchWorkspaces();
    await fetchSessions(workspaceId);
  } catch (err) {
    reportError(err);
  }

  renderWorkspaceBar();

  const session = state.sessions.find(
    (s) => String(s.sessionId ?? s.session_id) === sessionId,
  ) ?? null;
  const mtop = byId('cs-mtop');
  if (mtop) {
    mtop.querySelector('.t1').textContent = String(session?.title ?? t('session_untitled'));
    mtop.querySelector('.t2').textContent = String(session?.branch ?? '');
  }

  const host = byId('cs-session-host');
  if (!host) return;
  let mod;
  try {
    mod = await loadSessionModule();
  } catch (err) {
    host.innerHTML = `<div class="cs-empty">${sprite('alert')}<p>${escapeHtml(t('session_module_error'))}</p><p>${escapeHtml(describeError(err))}</p></div>`;
    return;
  }
  try {
    mod.mountSession(host, {
      workspaceId,
      sessionId,
      workspace: state.workspace,
      session,
      onExit: () => goto(workspaceId, null),
    });
  } catch (err) {
    host.innerHTML = `<div class="cs-empty">${sprite('alert')}<p>${escapeHtml(describeError(err))}</p></div>`;
    return;
  }

  // Owner-node reachability is a projection over the live console, never a
  // stored status (§3.5, §19): the watcher owns the session stream, hands every
  // event to the console and draws G01 when the node stops answering.
  const nodeId = String(state.workspace?.nodeId ?? state.workspace?.node_id ?? '');
  attachConnection({
    workspaceId,
    sessionId,
    nodeId,
    nodeLabel: nodeById(nodeId)?.name ?? nodeId,
    applyEvent: (event) => mod.applyEvent(event),
    onLeave: () => goto(null, null),
  });
}

// =============================================================================
// Screen object
// =============================================================================

const CodeStudioScreen = {
  get title() { return t('title'); },

  render() {
    return `
      <div id="cs-list-view">${listShellHtml()}</div>
      <div id="cs-workspace-view" hidden></div>
      <div class="cs-session-view" id="cs-session-view" hidden></div>
    `;
  },

  async mount(params = {}) {
    const me = await ApiBinary.one('authMeRequest').catch(() => null);
    state.isAdmin = String(me?.role ?? 'user').toLowerCase() === 'admin';

    state.narrow = window.matchMedia(NARROW_QUERY).matches;
    const media = window.matchMedia(NARROW_QUERY);
    state.narrowListener = (e) => {
      // The table is rebuilt only when the breakpoint actually flips, so a plain
      // resize never costs the user their scroll position or selection.
      if (state.narrow === e.matches) return;
      state.narrow = e.matches;
      if (state.view === 'list') renderTable();
    };
    media.addEventListener('change', state.narrowListener);

    state.card = window.matchMedia(CARD_QUERY).matches;
    const cardMedia = window.matchMedia(CARD_QUERY);
    state.cardListener = (e) => {
      if (state.card === e.matches) return;
      state.card = e.matches;
      if (state.view === 'list') renderTable();
    };
    cardMedia.addEventListener('change', state.cardListener);

    state.hashListener = () => {
      const route = parseHash();
      if (!route) return;
      if (route.workspaceId !== state.workspaceId || route.sessionId !== state.sessionId) {
        goto(route.workspaceId, route.sessionId);
      }
    };
    window.addEventListener('hashchange', state.hashListener);

    wireListEvents();
    await refreshList();

    const route = parseHash();
    const workspaceId = params.workspaceId ?? route?.workspaceId ?? null;
    const sessionId = params.sessionId ?? route?.sessionId ?? null;
    if (workspaceId) await goto(workspaceId, sessionId);
    else writeHash(null, null);
  },

  async unmount() {
    stopProvisionTracking();
    closeAllWindows();
    await unmountSessionSurface();
    if (state.narrowListener) {
      window.matchMedia(NARROW_QUERY).removeEventListener('change', state.narrowListener);
      state.narrowListener = null;
    }
    if (state.cardListener) {
      window.matchMedia(CARD_QUERY).removeEventListener('change', state.cardListener);
      state.cardListener = null;
    }
    if (state.hashListener) {
      window.removeEventListener('hashchange', state.hashListener);
      state.hashListener = null;
    }
    if (parseHash()) window.history.replaceState(null, '', window.location.pathname + window.location.search);
    state.view = 'list';
    state.workspaceId = null;
    state.sessionId = null;
    state.workspace = null;
    state.members = [];
    state.provisioning = [];
    state.sessions = [];
    state.allowlist = [];
    state.indexStatus = null;
    state.search = '';
    state.listFilter = 'all';
    state.nodeFilter = 'all';
  },
};

export default CodeStudioScreen;
