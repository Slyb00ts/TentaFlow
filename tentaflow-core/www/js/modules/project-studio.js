// ===== File: project-studio.js — Project Studio ("Projekty"): list, wizard, knowledge, chat, members, settings, tests, tasks =====
//
// Phase-1+2 surface over MessageBody::ProjectStudioBody (binary protocol only,
// codec projectStudio* helpers). Screens: project list (P01), 3-step creation
// wizard (P02), project overview with KPI + activity (P03), knowledge sources
// with chunked upload and live ingest tracking (W01/W02), KB search (W03),
// source files with preview (W04), private per-user chat with citations (C01),
// members (X03), settings (X04), the reusable danger delete window (G01),
// plus Phase-2: manual test cases with versioned editor and CSV import
// (T01/T02), agent generation wizard + live review (T04/T05), suites (T06),
// manual runs (T07/T08), the tester execution desk (T09), run results (T11),
// reports (T14), tasks/defects (Z01/Z02) and the notification bell (G02).
// tf-* components only; every visible string comes from i18n project_studio.*.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast, formatBytes } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import { TfAgentActivity } from '/js/components/tf-agent-activity.js';
import { activityLabels } from '/js/lib/agent-activity-bridge.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-table.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-menu.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-detail-header.js';
import '/js/components/tf-section-card.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-progress-bar.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-file-input.js';
import '/js/components/tf-chat-bubble.js';
import '/js/components/tf-chat-composer.js';
import '/js/components/tf-spinner.js';
import '/js/components/tf-tag-input.js';
import '/js/components/tf-line-chart.js';
import '/js/components/tf-bar-chart.js';
import '/js/components/tf-pie-chart.js';

// Project roles ordered by capability; assignable set excludes 'owner'
// (ownership moves only via the explicit transfer action).
const ROLE_RANK = { viewer: 0, tester: 1, editor: 2, manager: 3, owner: 4 };
const ASSIGNABLE_ROLES = ['manager', 'editor', 'tester', 'viewer'];

// Wizard templates map 1:1 to the wire `template` field; modules are the
// initial toggles for step 2 (knowledge is always locked on).
const TEMPLATES = [
  { id: 'custom', icon: 'folder', modules: ['knowledge'] },
  { id: 'tests', icon: 'list', modules: ['knowledge', 'tests', 'tasks'] },
  { id: 'docs', icon: 'file-text', modules: ['knowledge', 'docs', 'chat'] },
  { id: 'tests_docs', icon: 'grid-2x2', modules: ['knowledge', 'tests', 'docs', 'tasks', 'chat'] },
];

const MODULE_DEFS = [
  { id: 'knowledge', icon: 'database', locked: true },
  { id: 'tests', icon: 'list' },
  { id: 'docs', icon: 'file-text' },
  { id: 'chat', icon: 'message' },
  { id: 'tasks', icon: 'check' },
];

// F1 source kinds: document + url are functional, the rest is announced but
// disabled (backend rejects them with BadRequest until F3).
const SOURCE_KINDS = [
  { id: 'document', icon: 'file-text', enabled: true },
  { id: 'url', icon: 'globe', enabled: true },
  { id: 'git', icon: 'branch', enabled: false },
  { id: 'zip', icon: 'folder', enabled: false },
  { id: 'api_spec', icon: 'code', enabled: false },
];

const SOURCE_KIND_ICON = { document: 'file-text', url: 'globe', git: 'branch', zip: 'folder', api_spec: 'code' };
const SOURCE_STATUS_CHIP = { pending: 'info', indexing: 'accent', ready: 'ok', error: 'err', cancelled: 'warn' };
const FILE_STATUS_CHIP = { pending: 'info', indexing: 'accent', ready: 'ok', skipped: 'warn', error: 'err' };

const UPLOAD_CHUNK_BYTES = 1024 * 1024;
const FILES_PAGE_SIZE = 50;
const INGEST_POLL_MS = 3000;
const INGEST_LOG_CAP = 200;

const state = {
  me: null,
  isAdmin: false,
  // P01
  canCreate: false,
  projects: [],
  listFilter: 'active',
  searchQuery: '',
  // Open project context
  project: null,
  tab: 'overview',
  // P03
  activity: [],
  activityHasMore: false,
  // Knowledge
  kbView: 'sources',
  sources: [],
  // jobId -> { job, sourceId, log: [], unsub } — live ingest tracking; the
  // 3 s status poll is the source of truth, the stream only feeds the log.
  jobs: new Map(),
  jobsPollTimer: null,
  kbQuery: '',
  kbSelectedSources: new Set(),
  kbHits: null,
  kbSearching: false,
  files: { sourceId: null, offset: 0, filter: '', rows: [], total: 0 },
  // Chat
  chats: [],
  chatId: null,
  chatMessages: [],
  chatBusy: false,
  chatUnsub: null,
  // Members / settings
  members: [],
  memberQuery: '',
  memberRoleFilter: 'all',
  settings: null,
  agentOptions: [],
  // Open tf-window cleanups (wizard, source, invite, delete, preview, prompt).
  wins: new Set(),
  // F2 — tests module (T01–T14). `view` also carries drill-in sub-views
  // (case-editor, suite-editor, run-detail, exec, gen-detail) that map back
  // onto their parent segment in the sub-nav.
  f2: null,
  // F2 — tasks module (Z01/Z02).
  tasksView: null,
  // G02 — unread badge count (source of truth: NotificationsListRequest).
  notifUnread: 0,
};

// Fresh F2 state for every opened project — drill-in views, pollers and
// selections never leak between projects.
function freshF2State() {
  return {
    view: 'cases',
    tags: [],
    tagsLoaded: false,
    membersCache: null,
    cases: {
      rows: [], total: 0, page: 1,
      filters: { kind: '', status: '', priority: '', tagId: '', origin: '', search: '' },
      selected: new Set(),
    },
    editor: null,
    suites: [],
    suiteEditor: null,
    runs: { rows: [], total: 0, page: 1, status: '' },
    runDetail: null,
    exec: null,
    gens: [],
    genDetail: null,
    genUnsub: null,
    genPollTimer: null,
    genWidget: null,
    genSteps: [],
    reports: { from: '', to: '', suiteId: '', loaded: false, data: {} },
  };
}

function freshTasksState() {
  return {
    rows: [], total: 0, page: 1,
    filters: { type: '', status: '', mine: false, search: '' },
  };
}

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function t(key, params) {
  return I18n.t(`project_studio.${key}`, params);
}

function formatTimestamp(value) {
  if (!value) return '—';
  // SQLite datetime('now') yields "YYYY-MM-DD HH:MM:SS" in UTC, no zone marker.
  const iso = String(value).includes('T') ? String(value) : `${String(value).replace(' ', 'T')}Z`;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return String(value);
  return date.toLocaleString(I18n.getLanguage());
}

function initials(name) {
  const parts = String(name || '').trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return '?';
  return parts.slice(0, 2).map((p) => p[0]).join('').toUpperCase();
}

// AuthMe user_id arrives as Uint8Array(16); project rows carry string ids
// (hex/uuid). Compare on the normalized hex form.
function isMe(userId) {
  const bytes = state.me?.userId;
  if (!bytes || !userId) return false;
  const hex = Array.from(bytes).map((b) => b.toString(16).padStart(2, '0')).join('');
  return String(userId).toLowerCase().replace(/-/g, '') === hex;
}

function roleLabel(role) {
  return ROLE_RANK[role] !== undefined ? t(`role_${role}`) : String(role || '—');
}

function myRole() {
  const role = state.project?.my_role ?? state.project?.myRole;
  if (role) return role;
  // Org admin inspecting a project they are not a member of gets my_role=None;
  // the server still authorizes every call, the UI just unlocks manager tools.
  return state.isAdmin ? 'manager' : 'viewer';
}

function roleRank() {
  return ROLE_RANK[myRole()] ?? 0;
}

const canEdit = () => roleRank() >= ROLE_RANK.editor;
const canManage = () => roleRank() >= ROLE_RANK.manager;
const canTest = () => roleRank() >= ROLE_RANK.tester;
const isOwner = () => myRole() === 'owner';

function orientationLabel(project) {
  const template = project.template;
  if (template === 'tests') return t('orientation_tests');
  if (template === 'docs') return t('orientation_docs');
  if (template === 'tests_docs') return t('orientation_tests_docs');
  const modules = Array.isArray(project.modules) ? project.modules : [];
  const labels = modules.filter((m) => m !== 'knowledge').map((m) => t(`module_${m}`));
  return labels.length ? labels.join(' + ') : t('orientation_custom');
}

// =============================================================================
// Screen object
// =============================================================================

const ProjectStudioScreen = {
  get title() { return t('title'); },

  render() {
    return `
      <div id="ps-list-view">
        <div class="page-header">
          <div>
            <h1>${sprite('folder')} ${escapeHtml(t('title'))}</h1>
            <div class="sub" id="ps-list-sub"></div>
          </div>
          <div class="actions">
            <tf-searchbox id="ps-search" placeholder="${escapeAttr(t('search_placeholder'))}" debounce="200"></tf-searchbox>
            <tf-button variant="ghost" icon="refresh" id="ps-refresh">${escapeHtml(t('refresh'))}</tf-button>
            ${bellHtml()}
            <tf-button variant="primary" icon="plus" id="ps-new" hidden>${escapeHtml(t('new_project'))}</tf-button>
          </div>
        </div>
        <div class="ps-filters-row">
          <tf-filter-chips id="ps-filter" mode="single"></tf-filter-chips>
          <span class="ps-create-hint" id="ps-create-hint" hidden>${sprite('check')}${escapeHtml(t('can_create_hint'))}</span>
        </div>
        <div id="ps-grid-host"><div class="ps-loading">${escapeHtml(t('loading'))}</div></div>
      </div>
      <div id="ps-project-view" hidden></div>
    `;
  },

  async mount() {
    const me = await ApiBinary.one('authMeRequest').catch(() => null);
    state.me = me;
    state.isAdmin = (me?.role ?? 'user').toLowerCase() === 'admin';

    byId('ps-refresh')?.addEventListener('click', () => loadProjects());
    byId('ps-new')?.addEventListener('click', () => openWizard());
    byId('ps-search')?.addEventListener('search', (e) => {
      state.searchQuery = String(e.detail?.value ?? '');
      renderProjectGrid();
    });
    const filter = byId('ps-filter');
    if (filter) {
      filter.filters = [
        { id: 'active', label: t('filter_active'), active: state.listFilter === 'active' },
        { id: 'archived', label: t('filter_archived'), active: state.listFilter === 'archived' },
        { id: 'all', label: t('filter_all'), active: state.listFilter === 'all' },
      ];
      filter.addEventListener('change', (e) => {
        state.listFilter = e.detail?.id ?? 'active';
        loadProjects();
      });
    }
    wireGridEvents();
    wireBellEvents(byId('ps-list-view'));
    installNotifListener();
    refreshNotifBadge();
    await loadProjects();
  },

  unmount() {
    closeAllWindows();
    stopAllJobTracking();
    stopChatStream();
    stopTestsLive();
    state.projects = [];
    state.project = null;
    state.tab = 'overview';
    state.kbView = 'sources';
    state.sources = [];
    state.kbHits = null;
    state.kbQuery = '';
    state.kbSelectedSources = new Set();
    state.files = { sourceId: null, offset: 0, filter: '', rows: [], total: 0 };
    state.chats = [];
    state.chatId = null;
    state.chatMessages = [];
    state.members = [];
    state.settings = null;
    state.searchQuery = '';
    state.listFilter = 'active';
    state.f2 = null;
    state.tasksView = null;
  },
};

export default ProjectStudioScreen;

function closeAllWindows() {
  for (const cleanup of [...state.wins]) {
    try { cleanup(); } catch { /* window already gone */ }
  }
  state.wins.clear();
}

// =============================================================================
// Generic tf-window scaffolding (wizard, source, invite, delete, prompt)
// =============================================================================

function openWindow({ title, subtitle, icon, width = 640 }) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', title);
  if (subtitle) win.setAttribute('subtitle', subtitle);
  win.setAttribute('icon', icon || 'folder');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', String(width));
  win.setAttribute('draggable', '');

  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'ps-window-body';
  win.appendChild(body);

  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.className = 'ps-window-footer';
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

// Small modal prompt built on tf-window + tf-input (rename flows). Resolves
// with the trimmed value or null on cancel.
function openPromptWindow({ title, label, value = '', icon = 'edit' }) {
  return new Promise((resolve) => {
    const { body, foot, cleanup } = openWindow({ title, icon, width: 440 });
    let settled = false;
    const done = (result) => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve(result);
    };
    body.innerHTML = `<tf-input id="ps-prompt-input" label="${escapeAttr(label)}" value="${escapeAttr(value)}"></tf-input>`;
    foot.innerHTML = `
      <div class="ps-footer-left"></div>
      <div class="ps-footer-right">
        <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
        <tf-button variant="primary" icon="check" data-action="save">${escapeHtml(t('action_save'))}</tf-button>
      </div>
    `;
    foot.addEventListener('click', (e) => {
      const btn = e.target.closest('[data-action]');
      if (!btn) return;
      if (btn.dataset.action === 'cancel') done(null);
      else done(String(body.querySelector('#ps-prompt-input')?.value ?? '').trim());
    });
  });
}

// G01 — reusable danger confirmation window. `requireName` forces the operator
// to retype the exact name before the destructive button unlocks.
function openDeleteWindow({ title, targetName, targetSub, targetIcon = 'folder', warning, items = [], requireName = null, confirmLabel, onConfirm }) {
  const { body, foot, cleanup } = openWindow({ title, icon: 'alert', width: 540 });

  const itemsHtml = items.map((item) => `
    <div class="ps-del-item">
      <div class="ps-del-item-ico">${sprite(item.icon)}</div>
      <div>
        <div class="ps-del-item-name">${escapeHtml(item.name)}</div>
        ${item.sub ? `<div class="ps-del-item-sub">${escapeHtml(item.sub)}</div>` : ''}
      </div>
    </div>
  `).join('');

  body.innerHTML = `
    <div class="ps-del-target">
      <div class="ps-del-target-ico">${sprite(targetIcon)}</div>
      <div>
        <div class="ps-del-target-name">${escapeHtml(targetName)}</div>
        ${targetSub ? `<div class="ps-del-target-sub">${escapeHtml(targetSub)}</div>` : ''}
      </div>
    </div>
    <div class="ps-del-banner">${sprite('alert')}<span>${escapeHtml(warning)}</span></div>
    ${items.length ? `<div class="ps-del-label">${escapeHtml(t('delete_will_remove'))}</div><div class="ps-del-list">${itemsHtml}</div>` : ''}
    ${requireName ? `
      <div class="ps-field">
        <tf-input id="ps-del-confirm" label="${escapeAttr(t('delete_retype_label'))}" placeholder="${escapeAttr(requireName)}"></tf-input>
        <div class="ps-field-hint" data-del-hint>${escapeHtml(t('delete_retype_hint', { name: requireName }))}</div>
      </div>
    ` : ''}
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="danger-solid" icon="trash" data-action="confirm" ${requireName ? 'disabled' : ''}>${escapeHtml(confirmLabel)}</tf-button>
    </div>
  `;

  if (requireName) {
    body.querySelector('#ps-del-confirm')?.addEventListener('input', (e) => {
      const matches = String(e.target.value ?? '').trim() === requireName;
      const btn = foot.querySelector('[data-action="confirm"]');
      if (matches) btn?.removeAttribute('disabled');
      else btn?.setAttribute('disabled', '');
    });
  }

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || btn.hasAttribute('disabled')) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    btn.setAttribute('disabled', '');
    try {
      await onConfirm();
      cleanup();
    } catch (err) {
      btn.removeAttribute('disabled');
      toast(`${t('delete_failed')}: ${err.message}`, 'error');
    }
  });
}

// =============================================================================
// P01 — project list
// =============================================================================

async function loadProjects() {
  try {
    const resp = await ApiBinary.one('projectStudioProjectsListRequest', {
      includeArchived: state.listFilter !== 'active',
    });
    state.projects = Array.isArray(resp.projects) ? resp.projects : [];
    state.canCreate = !!(resp.canCreate ?? resp.can_create);
  } catch (err) {
    toast(`${t('load_failed')}: ${err.message}`, 'error');
    return;
  }
  const newBtn = byId('ps-new');
  if (newBtn) newBtn.hidden = !state.canCreate;
  const hint = byId('ps-create-hint');
  if (hint) hint.hidden = !state.canCreate;
  const sub = byId('ps-list-sub');
  if (sub) {
    const active = state.projects.filter((p) => p.status === 'active').length;
    const archived = state.projects.filter((p) => p.status === 'archived').length;
    sub.textContent = t('subtitle_stats', { count: state.projects.length, active, archived });
  }
  renderProjectGrid();
}

function visibleProjects() {
  const query = state.searchQuery.trim().toLowerCase();
  return state.projects.filter((p) => {
    if (state.listFilter === 'active' && p.status !== 'active') return false;
    if (state.listFilter === 'archived' && p.status !== 'archived') return false;
    if (query) {
      const haystack = `${p.name} ${p.description || ''}`.toLowerCase();
      if (!haystack.includes(query)) return false;
    }
    return true;
  });
}

function projectCardHtml(project) {
  const projectId = project.project_id ?? project.projectId;
  const archived = project.status === 'archived';
  const role = project.my_role ?? project.myRole;
  const memberCount = project.member_count ?? project.memberCount ?? 0;
  const sourceCount = project.source_count ?? project.sourceCount ?? 0;
  const sourcesReady = project.sources_ready ?? project.sourcesReady ?? 0;
  const projectRank = ROLE_RANK[role] ?? (state.isAdmin ? ROLE_RANK.manager : 0);
  const menuItems = [
    `<tf-menu-item action="open" icon="external-link">${escapeHtml(t('action_open'))}</tf-menu-item>`,
  ];
  if (projectRank >= ROLE_RANK.manager) {
    menuItems.push(archived
      ? `<tf-menu-item action="unarchive" icon="refresh">${escapeHtml(t('action_unarchive'))}</tf-menu-item>`
      : `<tf-menu-item action="archive" icon="clock">${escapeHtml(t('action_archive'))}</tf-menu-item>`);
  }
  if (role === 'owner' || (state.isAdmin && !role)) {
    menuItems.push('<tf-menu-divider></tf-menu-divider>');
    menuItems.push(`<tf-menu-item action="delete" icon="trash" danger>${escapeHtml(t('action_delete'))}</tf-menu-item>`);
  }
  return `
    <div class="ps-card ${archived ? 'is-archived' : ''}" data-project-id="${escapeAttr(projectId)}" role="button" tabindex="0">
      <div class="ps-card-top">
        <div class="ps-card-ico">${sprite('folder')}</div>
        <div class="ps-card-heading">
          <div class="ps-card-name">${escapeHtml(project.name)}</div>
          <div class="ps-card-desc">${escapeHtml(project.description || '')}</div>
        </div>
        <tf-chip status="${archived ? 'warn' : 'ok'}" dot>${escapeHtml(t(archived ? 'status_archived' : 'status_active'))}</tf-chip>
      </div>
      <div class="ps-card-stats">
        <span class="ps-card-stat">${sprite('users')}<b>${memberCount}</b>&nbsp;${escapeHtml(t('stat_members'))}</span>
        <span class="ps-card-stat">${sprite('database')}<b>${sourcesReady}/${sourceCount}</b>&nbsp;${escapeHtml(t('stat_sources'))}</span>
      </div>
      <div class="ps-card-foot">
        ${role ? `<tf-chip status="accent">${escapeHtml(t('your_role'))}: ${escapeHtml(roleLabel(role))}</tf-chip>` : ''}
        <tf-chip status="info">${escapeHtml(orientationLabel(project))}</tf-chip>
        <div class="ps-card-menu-wrap">
          <tf-button variant="ghost" size="sm" icon="chevron-down" data-more title="${escapeAttr(t('action_more'))}"></tf-button>
          <tf-menu placement="bottom-end" data-card-menu>${menuItems.join('')}</tf-menu>
        </div>
      </div>
    </div>
  `;
}

function renderProjectGrid() {
  const host = byId('ps-grid-host');
  if (!host) return;
  const visible = visibleProjects();
  if (!visible.length && !state.canCreate) {
    host.innerHTML = `<tf-empty-state icon="folder" title="${escapeAttr(t('empty_list'))}"></tf-empty-state>`;
    return;
  }
  const addCard = state.canCreate ? `
    <div class="ps-card ps-card-add" data-add-card role="button" tabindex="0">
      <div>
        <div class="ps-card-add-ico">${sprite('plus')}</div>
        <div class="ps-card-add-name">${escapeHtml(t('new_project'))}</div>
        <div class="ps-card-add-sub">${escapeHtml(t('add_card_sub'))}</div>
      </div>
    </div>
  ` : '';
  host.innerHTML = `<div class="ps-grid">${visible.map(projectCardHtml).join('')}${addCard}</div>`;
}

// One delegated listener survives every grid re-render (tf-menu requires the
// menus to be statically present in each card's HTML).
function wireGridEvents() {
  const host = byId('ps-grid-host');
  if (!host) return;

  host.addEventListener('click', (e) => {
    const more = e.target.closest('[data-more]');
    if (more) {
      e.stopPropagation();
      more.parentElement?.querySelector('[data-card-menu]')?.toggle();
      return;
    }
    if (e.target.closest('[data-card-menu]')) return;
    if (e.target.closest('[data-add-card]')) {
      openWizard();
      return;
    }
    const card = e.target.closest('[data-project-id]');
    if (card) openProject(card.dataset.projectId);
  });

  host.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    if (e.target.closest('[data-add-card]')) {
      e.preventDefault();
      openWizard();
      return;
    }
    const card = e.target.closest('[data-project-id]');
    if (card && e.target === card) {
      e.preventDefault();
      openProject(card.dataset.projectId);
    }
  });

  host.addEventListener('action', (e) => {
    const card = e.target.closest('[data-project-id]');
    if (!card || !e.target.closest('[data-card-menu]')) return;
    const projectId = card.dataset.projectId;
    const project = state.projects.find((p) => (p.project_id ?? p.projectId) === projectId);
    switch (e.detail?.action) {
      case 'open': openProject(projectId); break;
      case 'archive': setProjectArchived(projectId, true); break;
      case 'unarchive': setProjectArchived(projectId, false); break;
      case 'delete': if (project) confirmDeleteProject(project, { fromList: true }); break;
      default: break;
    }
  });
}

async function setProjectArchived(projectId, archived) {
  try {
    await ApiBinary.one('projectStudioProjectArchiveRequest', { projectId, archived });
    toast(t(archived ? 'archive_ok' : 'unarchive_ok'), 'success');
    await loadProjects();
    if (state.project && (state.project.project_id ?? state.project.projectId) === projectId) {
      await refreshProjectHeader();
    }
  } catch (err) {
    toast(`${t('archive_failed')}: ${err.message}`, 'error');
  }
}

function confirmDeleteProject(project, { fromList = false } = {}) {
  const projectId = project.project_id ?? project.projectId;
  const sourceCount = project.source_count ?? project.sourceCount ?? 0;
  const memberCount = project.member_count ?? project.memberCount ?? 0;
  openDeleteWindow({
    title: t('delete_project_title'),
    targetName: project.name,
    targetSub: t('delete_project_sub', { members: memberCount, role: roleLabel(project.my_role ?? project.myRole ?? myRole()) }),
    warning: t('delete_project_warning'),
    items: [
      { icon: 'database', name: t('delete_item_kb'), sub: t('delete_item_kb_sub', { count: sourceCount }) },
      { icon: 'message', name: t('delete_item_chats'), sub: t('delete_item_chats_sub') },
      { icon: 'users', name: t('delete_item_members'), sub: t('delete_item_members_sub', { count: memberCount }) },
    ],
    requireName: project.name,
    confirmLabel: t('delete_forever'),
    onConfirm: async () => {
      await ApiBinary.one('projectStudioProjectDeleteRequest', { projectId });
      toast(t('delete_project_ok'), 'success');
      if (!fromList) closeProject();
      await loadProjects();
    },
  });
}

// =============================================================================
// P02 — creation wizard (3 steps in a tf-window)
// =============================================================================

function openWizard() {
  const { body, foot, cleanup } = openWindow({
    title: t('wizard_title'),
    icon: 'folder',
    width: 720,
  });

  const wz = {
    step: 1,
    template: 'tests',
    modules: new Set(TEMPLATES.find((tp) => tp.id === 'tests').modules),
    // [{ userId, displayName, email, role }] — the creator becomes owner
    // server-side and is rendered as a fixed first row.
    members: [],
    candidates: [],
  };

  body.innerHTML = `
    <div class="ps-stepper">
      <div class="ps-step" data-step-pill="1"><span class="ps-step-n">1</span>${escapeHtml(t('wizard_step1'))}</div>
      <div class="ps-step-line" data-step-line="1"></div>
      <div class="ps-step" data-step-pill="2"><span class="ps-step-n">2</span>${escapeHtml(t('wizard_step2'))}</div>
      <div class="ps-step-line" data-step-line="2"></div>
      <div class="ps-step" data-step-pill="3"><span class="ps-step-n">3</span>${escapeHtml(t('wizard_step3'))}</div>
    </div>

    <div data-step-panel="1">
      <div class="ps-field" style="margin-bottom:12px;">
        <tf-input id="ps-wz-name" label="${escapeAttr(t('wizard_name_label'))}" hint="${escapeAttr(t('wizard_name_hint'))}"></tf-input>
      </div>
      <div class="ps-field" style="margin-bottom:12px;">
        <tf-textarea id="ps-wz-desc" label="${escapeAttr(t('wizard_desc_label'))}" rows="3" hint="${escapeAttr(t('wizard_desc_hint'))}"></tf-textarea>
      </div>
      <div class="ps-field">
        <span class="ps-field-label">${escapeHtml(t('wizard_template_label'))}</span>
        <div class="ps-choice-grid" data-template-grid>
          ${TEMPLATES.map((tp) => `
            <div class="ps-choice-card ${wz.template === tp.id ? 'is-selected' : ''}" data-template="${escapeAttr(tp.id)}" role="button" tabindex="0">
              <div class="ps-cc-ico">${sprite(tp.icon)}</div>
              <div>
                <div class="ps-cc-name">${escapeHtml(t(`tpl_${tp.id}_name`))}</div>
                <div class="ps-cc-desc">${escapeHtml(t(`tpl_${tp.id}_desc`))}</div>
              </div>
            </div>
          `).join('')}
        </div>
        <div class="ps-field-hint">${escapeHtml(t('wizard_template_hint'))}</div>
      </div>
    </div>

    <div data-step-panel="2" hidden>
      <div class="ps-field-hint" style="margin-bottom:10px;">${escapeHtml(t('wizard_modules_hint'))}</div>
      <div style="display:flex; flex-direction:column; gap:8px;" data-modules-host></div>
    </div>

    <div data-step-panel="3" hidden>
      <div class="ps-wizard-team-bar">
        <tf-searchbox id="ps-wz-member-search" placeholder="${escapeAttr(t('wizard_member_search'))}" debounce="250"></tf-searchbox>
        <tf-select id="ps-wz-member-role" value="tester">
          ${ASSIGNABLE_ROLES.map((r) => `<option value="${r}" ${r === 'tester' ? 'selected' : ''}>${escapeHtml(roleLabel(r))}</option>`).join('')}
        </tf-select>
      </div>
      <div class="ps-candidate-list" data-candidates hidden></div>
      <div data-team-host></div>
      <div class="ps-field-hint">${escapeHtml(t('wizard_team_hint'))}</div>
    </div>

    <div class="ps-form-error" data-form-error hidden></div>
  `;

  foot.innerHTML = `
    <div class="ps-footer-left">
      <tf-button variant="ghost" icon="chevron-left" data-action="back">${escapeHtml(t('wizard_back'))}</tf-button>
    </div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="primary" data-action="next"></tf-button>
    </div>
  `;

  const showError = (message) => {
    const el = body.querySelector('[data-form-error]');
    if (!el) return;
    el.hidden = !message;
    el.textContent = message || '';
  };

  const renderModules = () => {
    const host = body.querySelector('[data-modules-host]');
    if (!host) return;
    host.innerHTML = MODULE_DEFS.map((mod) => `
      <div class="ps-module-row">
        <div class="ps-source-ico">${sprite(mod.icon)}</div>
        <div class="ps-module-main">
          <div class="ps-module-name">
            ${escapeHtml(t(`module_${mod.id}`))}
            ${mod.locked ? `<tf-chip status="accent">${escapeHtml(t('module_required'))}</tf-chip>` : ''}
          </div>
          <div class="ps-module-desc">${escapeHtml(t(`module_${mod.id}_desc`))}</div>
        </div>
        <tf-toggle data-module="${escapeAttr(mod.id)}" ${wz.modules.has(mod.id) ? 'checked' : ''} ${mod.locked ? 'disabled' : ''}></tf-toggle>
      </div>
    `).join('');
  };

  const renderTeam = () => {
    const host = body.querySelector('[data-team-host]');
    if (!host) return;
    const selfRow = `
      <div class="ps-member-row">
        <div class="ps-av-mini">${escapeHtml(initials(state.me?.username))}</div>
        <div class="ps-member-main">
          <div class="ps-member-name">${escapeHtml(state.me?.username || '')} <tf-chip status="accent">${escapeHtml(t('you_chip'))}</tf-chip></div>
        </div>
        <tf-chip status="info">${escapeHtml(roleLabel('owner'))}</tf-chip>
      </div>
    `;
    const rows = wz.members.map((m, i) => `
      <div class="ps-member-row">
        <div class="ps-av-mini">${escapeHtml(initials(m.displayName))}</div>
        <div class="ps-member-main">
          <div class="ps-member-name">${escapeHtml(m.displayName)}</div>
          <div class="ps-member-mail">${escapeHtml(m.email || '')}</div>
        </div>
        <tf-select class="ps-member-role" data-team-role="${i}" value="${escapeAttr(m.role)}">
          ${ASSIGNABLE_ROLES.map((r) => `<option value="${r}" ${r === m.role ? 'selected' : ''}>${escapeHtml(roleLabel(r))}</option>`).join('')}
        </tf-select>
        <tf-button variant="ghost" size="sm" icon="trash" data-team-remove="${i}" title="${escapeAttr(t('wizard_member_remove'))}"></tf-button>
      </div>
    `).join('');
    host.innerHTML = selfRow + rows;
  };

  const renderCandidates = () => {
    const host = body.querySelector('[data-candidates]');
    if (!host) return;
    if (!wz.candidates.length) {
      host.hidden = true;
      host.innerHTML = '';
      return;
    }
    const chosen = new Set(wz.members.map((m) => m.userId));
    host.hidden = false;
    host.innerHTML = wz.candidates.map((u) => {
      const userId = u.user_id ?? u.userId;
      if (chosen.has(userId) || isMe(userId)) return '';
      return `
        <div class="ps-candidate-row" data-candidate="${escapeAttr(userId)}" role="button" tabindex="0">
          <div class="ps-av-mini">${escapeHtml(initials(u.display_name ?? u.displayName))}</div>
          <div>
            <div class="ps-candidate-name">${escapeHtml(u.display_name ?? u.displayName ?? '')}</div>
            <div class="ps-candidate-mail">${escapeHtml(u.email || '')}</div>
          </div>
        </div>
      `;
    }).join('');
  };

  const setStep = (step) => {
    wz.step = step;
    body.querySelectorAll('[data-step-panel]').forEach((panel) => {
      panel.hidden = Number(panel.dataset.stepPanel) !== step;
    });
    body.querySelectorAll('[data-step-pill]').forEach((pill) => {
      const n = Number(pill.dataset.stepPill);
      pill.classList.toggle('is-active', n === step);
      pill.classList.toggle('is-done', n < step);
    });
    body.querySelectorAll('[data-step-line]').forEach((line) => {
      line.classList.toggle('is-done', Number(line.dataset.stepLine) < step);
    });
    const backBtn = foot.querySelector('[data-action="back"]');
    if (backBtn) backBtn.style.visibility = step > 1 ? 'visible' : 'hidden';
    const nextBtn = foot.querySelector('[data-action="next"]');
    if (nextBtn) {
      nextBtn.setAttribute('icon', step < 3 ? 'chevron-right' : 'check');
      nextBtn.setAttribute('label', step < 3 ? t('wizard_next') : t('wizard_create'));
    }
    showError(null);
  };

  const save = async () => {
    const name = String(body.querySelector('#ps-wz-name')?.value ?? '').trim();
    const description = String(body.querySelector('#ps-wz-desc')?.value ?? '').trim();
    try {
      const resp = await ApiBinary.one('projectStudioProjectCreateRequest', {
        name,
        description,
        template: wz.template,
        modules: [...wz.modules],
        members: wz.members.map((m) => ({ userId: m.userId, role: m.role })),
      });
      const projectId = resp.projectId ?? resp.project_id;
      toast(t('create_ok'), 'success');
      cleanup();
      await loadProjects();
      if (projectId) await openProject(projectId);
    } catch (err) {
      showError(`${t('create_failed')}: ${err.message}`);
    }
  };

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') {
      cleanup();
    } else if (btn.dataset.action === 'back') {
      if (wz.step > 1) setStep(wz.step - 1);
    } else if (btn.dataset.action === 'next') {
      if (wz.step === 1) {
        const name = String(body.querySelector('#ps-wz-name')?.value ?? '').trim();
        if (name.length < 3) {
          showError(t('err_name_short'));
          return;
        }
        setStep(2);
      } else if (wz.step === 2) {
        setStep(3);
      } else {
        await save();
      }
    }
  });

  body.querySelector('[data-template-grid]')?.addEventListener('click', (e) => {
    const card = e.target.closest('[data-template]');
    if (!card) return;
    wz.template = card.dataset.template;
    // Re-seed module toggles from the picked template; step 2 can still adjust.
    wz.modules = new Set(TEMPLATES.find((tp) => tp.id === wz.template)?.modules ?? ['knowledge']);
    body.querySelectorAll('[data-template]').forEach((c) => {
      c.classList.toggle('is-selected', c.dataset.template === wz.template);
    });
    renderModules();
  });

  body.querySelector('[data-modules-host]')?.addEventListener('change', (e) => {
    const toggle = e.target.closest('tf-toggle[data-module]');
    if (!toggle) return;
    const id = toggle.dataset.module;
    const checked = e.detail?.checked ?? toggle.hasAttribute('checked');
    if (checked) wz.modules.add(id);
    else wz.modules.delete(id);
  });

  body.querySelector('#ps-wz-member-search')?.addEventListener('search', async (e) => {
    const query = String(e.detail?.value ?? '').trim();
    if (!query) {
      wz.candidates = [];
      renderCandidates();
      return;
    }
    try {
      const resp = await ApiBinary.one('projectStudioMemberCandidatesRequest', { query, limit: 12 });
      wz.candidates = Array.isArray(resp.users) ? resp.users : [];
    } catch {
      wz.candidates = [];
    }
    renderCandidates();
  });

  body.addEventListener('click', (e) => {
    const candidate = e.target.closest('[data-candidate]');
    if (candidate) {
      const userId = candidate.dataset.candidate;
      const user = wz.candidates.find((u) => (u.user_id ?? u.userId) === userId);
      if (user) {
        const role = String(body.querySelector('#ps-wz-member-role')?.value || 'tester');
        wz.members.push({
          userId,
          displayName: user.display_name ?? user.displayName ?? '',
          email: user.email || '',
          role,
        });
        renderTeam();
        renderCandidates();
      }
      return;
    }
    const removeBtn = e.target.closest('[data-team-remove]');
    if (removeBtn) {
      wz.members.splice(Number(removeBtn.dataset.teamRemove), 1);
      renderTeam();
      renderCandidates();
    }
  });

  body.addEventListener('change', (e) => {
    const roleSel = e.target.closest('[data-team-role]');
    if (!roleSel) return;
    const idx = Number(roleSel.dataset.teamRole);
    if (wz.members[idx]) wz.members[idx].role = e.detail?.value ?? roleSel.value;
  });

  renderModules();
  renderTeam();
  setStep(1);
}

// =============================================================================
// Project shell — header + module tabs
// =============================================================================

// `deep` (optional) jumps straight into a tab after opening; used by the
// notification panel / "my test work" cross-project navigation:
// { tab, sub? (tests sub-view), runId?, genId?, taskId? }.
async function openProject(projectId, deep = null) {
  let project = null;
  try {
    const resp = await ApiBinary.one('projectStudioProjectGetRequest', { projectId });
    project = resp.project;
  } catch (err) {
    // NotFound (or any access error) returns to the list with a message.
    toast(`${t('project_open_failed')}: ${err.message}`, 'error');
    return;
  }
  if (!project) {
    toast(t('project_open_failed'), 'error');
    return;
  }
  stopAllJobTracking();
  stopChatStream();
  stopTestsLive();
  state.project = project;
  state.tab = 'overview';
  state.kbView = 'sources';
  state.kbHits = null;
  state.kbQuery = '';
  state.kbSelectedSources = new Set();
  state.chats = [];
  state.chatId = null;
  state.chatMessages = [];
  state.f2 = freshF2State();
  state.tasksView = freshTasksState();

  const listView = byId('ps-list-view');
  const projectView = byId('ps-project-view');
  if (listView) listView.hidden = true;
  if (!projectView) return;
  projectView.hidden = false;

  const modules = Array.isArray(project.modules) ? project.modules : [];
  if (deep?.tab && (deep.tab === 'overview' || modules.includes(deep.tab) || deep.tab === 'members')) {
    state.tab = deep.tab;
  }
  if (deep?.sub && state.tab === 'tests' && TESTS_SEGMENTS.includes(deep.sub)) {
    state.f2.view = deep.sub;
  }
  renderProjectShell();
  await switchTab(state.tab);
  if (deep?.runId && state.tab === 'tests') await openRunDetail(deep.runId);
  else if (deep?.genId && state.tab === 'tests') await openGenDetail(deep.genId);
  else if (deep?.taskId && state.tab === 'tasks') await openTaskWindow({ taskId: deep.taskId });
}

function closeProject() {
  stopAllJobTracking();
  stopChatStream();
  state.project = null;
  const listView = byId('ps-list-view');
  const projectView = byId('ps-project-view');
  if (projectView) { projectView.hidden = true; projectView.innerHTML = ''; }
  if (listView) listView.hidden = false;
  loadProjects();
}

async function refreshProjectHeader() {
  const projectId = state.project?.project_id ?? state.project?.projectId;
  if (!projectId) return;
  try {
    const resp = await ApiBinary.one('projectStudioProjectGetRequest', { projectId });
    if (resp.project) {
      state.project = resp.project;
      renderProjectShell();
      renderTabsValue();
      await switchTab(state.tab);
    }
  } catch {
    // Header refresh is cosmetic; a failed fetch keeps the current view.
  }
}

function enabledTabs() {
  const modules = Array.isArray(state.project?.modules) ? state.project.modules : [];
  const tabs = [{ id: 'overview', icon: 'chart-line' }];
  if (modules.includes('knowledge')) tabs.push({ id: 'knowledge', icon: 'database' });
  if (modules.includes('tests')) tabs.push({ id: 'tests', icon: 'list' });
  if (modules.includes('tasks')) tabs.push({ id: 'tasks', icon: 'check' });
  if (modules.includes('chat')) tabs.push({ id: 'chat', icon: 'message' });
  tabs.push({ id: 'members', icon: 'users' });
  if (canManage()) tabs.push({ id: 'settings', icon: 'settings' });
  return tabs;
}

function renderProjectShell() {
  const host = byId('ps-project-view');
  const project = state.project;
  if (!host || !project) return;
  const archived = project.status === 'archived';
  const memberCount = project.member_count ?? project.memberCount ?? 0;
  const sourceCount = project.source_count ?? project.sourceCount ?? 0;
  const tabs = enabledTabs();

  host.innerHTML = `
    <tf-breadcrumb class="ps-project-crumbs" id="ps-crumbs">
      <tf-breadcrumb-item href="#">${escapeHtml(t('title'))}</tf-breadcrumb-item>
      <tf-breadcrumb-item current>${escapeHtml(project.name)}</tf-breadcrumb-item>
    </tf-breadcrumb>

    <tf-detail-header title="${escapeAttr(project.name)}" subtitle="${escapeAttr(project.description || '')}" icon="folder">
      <span slot="badges">
        <tf-chip status="${archived ? 'warn' : 'ok'}" dot>${escapeHtml(t(archived ? 'status_archived' : 'status_active'))}</tf-chip>
        <tf-chip status="accent">${sprite('users')} ${memberCount} ${escapeHtml(t('stat_members'))}</tf-chip>
        <tf-chip>${escapeHtml(t('your_role'))}: ${escapeHtml(roleLabel(myRole()))}</tf-chip>
        <tf-chip status="info">${escapeHtml(t('modules_chip'))}: ${escapeHtml(orientationLabel(project))}</tf-chip>
        <tf-chip status="info">${sprite('database')} ${sourceCount} ${escapeHtml(t('stat_sources'))}</tf-chip>
      </span>
      <span slot="actions">
        <tf-button variant="ghost" icon="chevron-left" data-back-list>${escapeHtml(t('back_to_list'))}</tf-button>
        ${bellHtml()}
        <tf-button variant="ghost" icon="users" data-goto-members>${escapeHtml(t('tab_members'))}</tf-button>
        ${canManage() ? `<tf-button variant="ghost" icon="settings" data-goto-settings>${escapeHtml(t('tab_settings'))}</tf-button>` : ''}
      </span>
    </tf-detail-header>

    <tf-tabs variant="underline" value="${escapeAttr(state.tab)}" id="ps-project-tabs" class="ps-project-tabs">
      ${tabs.map((tab) => `<tf-tab id="${tab.id}" icon="${tab.icon}">${escapeHtml(t(`tab_${tab.id}`))}</tf-tab>`).join('')}
    </tf-tabs>

    <div id="ps-tab-panel"></div>
  `;

  // tf-breadcrumb re-renders items as anchors inside its own <nav>, so the
  // click handler must live on the container and match the rendered link.
  host.querySelector('#ps-crumbs')?.addEventListener('click', (e) => {
    const link = e.target.closest('a.tf-breadcrumb-item');
    if (!link) return;
    e.preventDefault();
    closeProject();
  });
  host.querySelector('[data-back-list]')?.addEventListener('click', () => closeProject());
  host.querySelector('[data-goto-members]')?.addEventListener('click', () => selectTab('members'));
  host.querySelector('[data-goto-settings]')?.addEventListener('click', () => selectTab('settings'));
  wireBellEvents(host);
  refreshNotifBadge();
  byId('ps-project-tabs')?.addEventListener('change', (e) => {
    const id = e.detail?.value;
    if (id && id !== state.tab) switchTab(id);
  });
}

function renderTabsValue() {
  byId('ps-project-tabs')?.setAttribute('value', state.tab);
}

function selectTab(tab) {
  if (state.tab === tab) return;
  switchTab(tab);
  renderTabsValue();
}

async function switchTab(tab) {
  state.tab = tab;
  // Leaving the chat tab must not leak the active stream subscription.
  if (tab !== 'chat') stopChatStream();
  if (tab !== 'knowledge') stopAllJobTracking();
  if (tab !== 'tests') stopTestsLive();
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  panel.innerHTML = `<div class="ps-loading">${escapeHtml(t('loading'))}</div>`;
  switch (tab) {
    case 'overview': await renderOverview(); break;
    case 'knowledge': await renderKnowledge(); break;
    case 'tests': await renderTests(); break;
    case 'tasks': await renderTasksTab(); break;
    case 'chat': await renderChat(); break;
    case 'members': await renderMembers(); break;
    case 'settings': await renderSettings(); break;
    default: panel.innerHTML = ''; break;
  }
}

function projectId() {
  return state.project?.project_id ?? state.project?.projectId;
}

// =============================================================================
// P03 — overview (KPI + activity + quick actions)
// =============================================================================

async function renderOverview() {
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  let kpis = null;
  try {
    const resp = await ApiBinary.one('projectStudioOverviewRequest', { projectId: projectId() });
    kpis = resp.kpis;
    state.activity = Array.isArray(resp.activity) ? resp.activity : [];
    state.activityHasMore = state.activity.length >= 20;
  } catch (err) {
    panel.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('overview_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'overview') return;

  const sourcesReady = kpis?.sources_ready ?? kpis?.sourcesReady ?? 0;
  const sourcesTotal = kpis?.sources_total ?? kpis?.sourcesTotal ?? 0;
  const openJobs = kpis?.open_ingest_jobs ?? kpis?.openIngestJobs ?? 0;
  const modules = Array.isArray(state.project?.modules) ? state.project.modules : [];

  const quickActions = [];
  if (modules.includes('knowledge') && canEdit()) {
    quickActions.push({ id: 'add-source', icon: 'database', name: t('qa_add_source'), sub: t('qa_add_source_sub') });
  }
  if (modules.includes('knowledge')) {
    quickActions.push({ id: 'kb-search', icon: 'search', name: t('qa_search'), sub: t('qa_search_sub') });
  }
  if (modules.includes('tests')) {
    quickActions.push({ id: 'tests', icon: 'list', name: t('qa_tests'), sub: t('qa_tests_sub') });
  }
  if (modules.includes('tasks')) {
    quickActions.push({ id: 'tasks', icon: 'check', name: t('qa_tasks'), sub: t('qa_tasks_sub') });
  }
  if (modules.includes('chat')) {
    quickActions.push({ id: 'chat', icon: 'message', name: t('qa_chat'), sub: t('qa_chat_sub') });
  }
  quickActions.push({ id: 'members', icon: 'users', name: t('qa_members'), sub: t('qa_members_sub') });

  const kv = (snake, camel) => Number(kpis?.[snake] ?? kpis?.[camel] ?? 0);
  const gensRunning = kv('generations_running', 'generationsRunning');
  const testsKpis = modules.includes('tests') ? `
      <tf-stat-card icon="list" label="${escapeAttr(t('kpi_cases'))}" value="${kv('cases_approved', 'casesApproved')}" suffix="/ ${kv('cases_total', 'casesTotal')}"
        delta="${escapeAttr(t('kpi_suites', { count: kv('suites_total', 'suitesTotal') }))}" delta-type="neutral"></tf-stat-card>
      <tf-stat-card icon="play" label="${escapeAttr(t('kpi_runs_open'))}" value="${kv('runs_open', 'runsOpen')}"
        ${gensRunning > 0 ? `delta="${escapeAttr(t('kpi_generations_running', { count: gensRunning }))}" delta-type="warn"` : ''}></tf-stat-card>
      <tf-stat-card icon="user" label="${escapeAttr(t('kpi_my_items'))}" value="${kv('my_run_items_pending', 'myRunItemsPending')}"></tf-stat-card>
  ` : '';
  const tasksKpis = modules.includes('tasks') ? `
      <tf-stat-card icon="check" label="${escapeAttr(t('kpi_tasks_open'))}" value="${kv('tasks_open', 'tasksOpen')}"
        ${kv('defects_open', 'defectsOpen') > 0 ? `delta="${escapeAttr(t('kpi_defects_open', { count: kv('defects_open', 'defectsOpen') }))}" delta-type="warn"` : ''}></tf-stat-card>
  ` : '';

  panel.innerHTML = `
    <div class="ps-kpi-grid">
      <tf-stat-card icon="database" label="${escapeAttr(t('kpi_sources'))}" value="${sourcesReady}" suffix="/ ${sourcesTotal}"
        ${openJobs > 0 ? `delta="${escapeAttr(t('kpi_open_jobs', { count: openJobs }))}" delta-type="warn"` : ''}></tf-stat-card>
      <tf-stat-card icon="file-text" label="${escapeAttr(t('kpi_files'))}" value="${kpis?.files_total ?? kpis?.filesTotal ?? 0}"></tf-stat-card>
      <tf-stat-card icon="grid-2x2" label="${escapeAttr(t('kpi_chunks'))}" value="${kpis?.chunks_total ?? kpis?.chunksTotal ?? 0}"></tf-stat-card>
      <tf-stat-card icon="users" label="${escapeAttr(t('kpi_members'))}" value="${kpis?.member_count ?? kpis?.memberCount ?? 0}"
        delta="${escapeAttr(t('kpi_my_chats', { count: kpis?.my_chat_count ?? kpis?.myChatCount ?? 0 }))}" delta-type="neutral"></tf-stat-card>
      ${testsKpis}
      ${tasksKpis}
    </div>

    <div class="ps-overview-cols">
      <tf-section-card title="${escapeAttr(t('activity_title'))}" icon="clock">
        <div class="ps-activity-feed" id="ps-activity-feed"></div>
        <div class="ps-activity-more" id="ps-activity-more" hidden>
          <tf-button variant="ghost" size="sm" icon="chevron-down" id="ps-activity-more-btn">${escapeHtml(t('activity_more'))}</tf-button>
        </div>
      </tf-section-card>
      <tf-section-card title="${escapeAttr(t('quick_actions_title'))}" icon="play">
        <div class="ps-quick-actions">
          ${quickActions.map((qa) => `
            <div class="ps-quick-action" data-qa="${escapeAttr(qa.id)}" role="button" tabindex="0">
              <div class="ps-qa-ico">${sprite(qa.icon)}</div>
              <div>
                <div class="ps-qa-name">${escapeHtml(qa.name)}</div>
                <div class="ps-qa-sub">${escapeHtml(qa.sub)}</div>
              </div>
            </div>
          `).join('')}
        </div>
      </tf-section-card>
    </div>
  `;

  renderActivityFeed();
  byId('ps-activity-more-btn')?.addEventListener('click', () => loadMoreActivity());
  panel.querySelectorAll('[data-qa]').forEach((el) => {
    el.addEventListener('click', () => {
      const id = el.dataset.qa;
      if (id === 'add-source') { selectTab('knowledge'); setTimeout(() => openSourceWindow(null), 0); }
      else if (id === 'kb-search') { state.kbView = 'search'; selectTab('knowledge'); }
      else if (id === 'tests') selectTab('tests');
      else if (id === 'tasks') selectTab('tasks');
      else if (id === 'chat') selectTab('chat');
      else if (id === 'members') selectTab('members');
    });
  });
}

function activityEntryHtml(entry) {
  const kind = entry.actor_kind ?? entry.actorKind;
  const actorName = entry.actor_name ?? entry.actorName ?? '';
  const av = kind === 'agent' || kind === 'system'
    ? `<div class="ps-activity-av is-agent">${sprite(kind === 'agent' ? 'sparkle' : 'settings')}</div>`
    : `<div class="ps-activity-av">${escapeHtml(initials(actorName))}</div>`;
  const action = String(entry.action || '');
  const actionKey = `activity_${action.replace(/\./g, '_')}`;
  const translated = I18n.t(`project_studio.${actionKey}`);
  const actionLabel = translated === `project_studio.${actionKey}` ? action : translated;
  return `
    <div class="ps-activity-item">
      ${av}
      <div>
        <div class="ps-activity-text"><b>${escapeHtml(actorName)}</b> ${escapeHtml(actionLabel)}${entry.object_id ?? entry.objectId ? ` · <span class="ps-kb-path">${escapeHtml(entry.object_type ?? entry.objectType ?? '')}</span>` : ''}</div>
        <div class="ps-activity-time">${escapeHtml(formatTimestamp(entry.created_at ?? entry.createdAt))}</div>
      </div>
    </div>
  `;
}

function renderActivityFeed() {
  const feed = byId('ps-activity-feed');
  if (!feed) return;
  if (!state.activity.length) {
    feed.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('activity_empty'))}</div>`;
  } else {
    feed.innerHTML = state.activity.map(activityEntryHtml).join('');
  }
  const more = byId('ps-activity-more');
  if (more) more.hidden = !state.activityHasMore;
}

async function loadMoreActivity() {
  const last = state.activity[state.activity.length - 1];
  if (!last) return;
  try {
    const resp = await ApiBinary.one('projectStudioActivityListRequest', {
      projectId: projectId(),
      beforeId: String(last.id),
      limit: 50,
    });
    const entries = Array.isArray(resp.entries) ? resp.entries : [];
    state.activity = state.activity.concat(entries);
    state.activityHasMore = !!(resp.hasMore ?? resp.has_more);
    renderActivityFeed();
  } catch (err) {
    toast(`${t('activity_failed')}: ${err.message}`, 'error');
  }
}

// =============================================================================
// Knowledge shell (W01/W03/W04 behind a segmented sub-nav)
// =============================================================================

async function renderKnowledge() {
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  panel.innerHTML = `
    <div class="ps-subnav">
      <tf-segmented id="ps-kb-view" value="${escapeAttr(state.kbView)}">
        <option value="sources">${escapeHtml(t('kb_view_sources'))}</option>
        <option value="search">${escapeHtml(t('kb_view_search'))}</option>
        <option value="files">${escapeHtml(t('kb_view_files'))}</option>
      </tf-segmented>
      <span class="ps-subnav-hint">${escapeHtml(t('kb_subnav_hint'))}</span>
    </div>
    <div id="ps-kb-host"></div>
  `;
  byId('ps-kb-view')?.addEventListener('change', (e) => {
    const view = e.detail?.value;
    if (!view || view === state.kbView) return;
    state.kbView = view;
    if (view !== 'sources') stopAllJobTracking();
    renderKnowledgeView();
  });
  await renderKnowledgeView();
}

async function renderKnowledgeView() {
  const host = byId('ps-kb-host');
  if (!host) return;
  host.innerHTML = `<div class="ps-loading">${escapeHtml(t('loading'))}</div>`;
  if (state.kbView === 'sources') await renderSources();
  else if (state.kbView === 'search') renderKbSearch();
  else await renderFilesView();
}

// =============================================================================
// W01 — sources list + live ingest
// =============================================================================

async function loadSources() {
  const resp = await ApiBinary.one('projectStudioSourcesListRequest', { projectId: projectId() });
  state.sources = Array.isArray(resp.sources) ? resp.sources : [];
}

function sourceRowHtml(source) {
  const sourceId = source.source_id ?? source.sourceId;
  const status = source.status;
  const job = source.last_job ?? source.lastJob;
  const jobId = job ? (job.job_id ?? job.jobId) : null;
  const running = job && job.status === 'running';
  const fileCount = source.file_count ?? source.fileCount ?? 0;
  const chunkCount = source.chunk_count ?? source.chunkCount ?? 0;
  const mutable = canEdit();

  const meta = [
    t(`kind_${source.kind}`),
    t('source_files_count', { count: fileCount }),
    t('source_chunks_count', { count: chunkCount }),
    `${t('source_added')} ${formatTimestamp(source.created_at ?? source.createdAt)}`,
    source.created_by_name ?? source.createdByName ?? '',
  ].filter(Boolean);

  const tracked = jobId ? state.jobs.get(jobId) : null;
  const liveJob = tracked?.job ?? job;
  const filesTotal = liveJob ? (liveJob.files_total ?? liveJob.filesTotal ?? 0) : 0;
  const filesDone = liveJob ? (liveJob.files_done ?? liveJob.filesDone ?? 0) : 0;
  const pct = filesTotal > 0 ? Math.round((filesDone / filesTotal) * 100) : 0;

  const ingestHtml = running ? `
    <div class="ps-source-ingest">
      <tf-progress-bar size="sm" value="${pct}" data-job-progress="${escapeAttr(jobId)}"></tf-progress-bar>
      <span class="ps-ingest-pct" data-job-pct="${escapeAttr(jobId)}">${pct}%</span>
      ${mutable ? `<tf-button variant="ghost" size="sm" icon="x" data-cancel-job="${escapeAttr(jobId)}">${escapeHtml(t('ingest_cancel'))}</tf-button>` : ''}
    </div>
    <div class="ps-ingest-log" data-job-log="${escapeAttr(jobId)}">${escapeHtml((tracked?.log ?? []).join('\n'))}</div>
  ` : '';

  const actions = [];
  if (mutable) {
    actions.push(`<tf-button variant="ghost" size="sm" icon="refresh" data-reingest="${escapeAttr(sourceId)}" title="${escapeAttr(t('action_reingest'))}"></tf-button>`);
    actions.push(`
      <tf-button variant="ghost" size="sm" icon="chevron-down" data-source-more title="${escapeAttr(t('action_more'))}"></tf-button>
      <tf-menu placement="bottom-end" data-source-menu>
        <tf-menu-item action="edit" icon="edit">${escapeHtml(t('action_edit'))}</tf-menu-item>
        <tf-menu-item action="reingest" icon="refresh">${escapeHtml(t('action_reingest'))}</tf-menu-item>
        <tf-menu-item action="files" icon="file-text">${escapeHtml(t('action_show_files'))}</tf-menu-item>
        <tf-menu-divider></tf-menu-divider>
        <tf-menu-item action="delete" icon="trash" danger>${escapeHtml(t('action_delete_source'))}</tf-menu-item>
      </tf-menu>
    `);
  } else {
    actions.push(`<tf-button variant="ghost" size="sm" icon="file-text" data-source-files="${escapeAttr(sourceId)}" title="${escapeAttr(t('action_show_files'))}"></tf-button>`);
  }

  return `
    <div class="ps-source-row ${status === 'error' ? 'is-error' : ''}" data-source-id="${escapeAttr(sourceId)}">
      <div class="ps-source-ico">${sprite(SOURCE_KIND_ICON[source.kind] || 'file-text')}</div>
      <div class="ps-source-main">
        <div class="ps-source-name">
          ${escapeHtml(source.name)}
          <tf-chip status="${SOURCE_STATUS_CHIP[status] || 'info'}" dot>${escapeHtml(t(`source_status_${status}`))}</tf-chip>
        </div>
        <div class="ps-source-meta">${meta.map((m) => `<span>${escapeHtml(m)}</span>`).join('')}</div>
        ${source.error ? `<div class="ps-source-error">${escapeHtml(source.error)}</div>` : ''}
        ${ingestHtml}
      </div>
      <div class="ps-source-actions">${actions.join('')}</div>
    </div>
  `;
}

async function renderSources() {
  const host = byId('ps-kb-host');
  if (!host) return;
  try {
    await loadSources();
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('sources_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'knowledge' || state.kbView !== 'sources') return;

  const ready = state.sources.filter((s) => s.status === 'ready').length;
  const indexing = state.sources.filter((s) => s.status === 'indexing' || s.status === 'pending').length;
  const errors = state.sources.filter((s) => s.status === 'error').length;

  host.innerHTML = `
    <tf-section-card title="${escapeAttr(t('sources_title'))}" icon="database">
      <span slot="subtitle">${escapeHtml(t('sources_stats', { ready, indexing, errors }))}</span>
      <span slot="actions">
        ${canEdit() ? `<tf-button variant="primary" size="sm" icon="plus" id="ps-add-source">${escapeHtml(t('add_source'))}</tf-button>` : ''}
      </span>
      <div id="ps-sources-list">
        ${state.sources.length
          ? state.sources.map(sourceRowHtml).join('')
          : `<tf-empty-state icon="database" title="${escapeAttr(t('sources_empty'))}"></tf-empty-state>`}
      </div>
    </tf-section-card>
  `;

  byId('ps-add-source')?.addEventListener('click', () => openSourceWindow(null));
  const list = byId('ps-sources-list');
  list?.addEventListener('click', (e) => {
    const more = e.target.closest('[data-source-more]');
    if (more) {
      e.stopPropagation();
      more.parentElement?.querySelector('[data-source-menu]')?.toggle();
      return;
    }
    const cancel = e.target.closest('[data-cancel-job]');
    if (cancel) { cancelIngest(cancel.dataset.cancelJob); return; }
    const reingest = e.target.closest('[data-reingest]');
    if (reingest) { reingestSource(reingest.dataset.reingest); return; }
    const filesBtn = e.target.closest('[data-source-files]');
    if (filesBtn) { showFilesFor(filesBtn.dataset.sourceFiles); }
  });
  list?.addEventListener('action', (e) => {
    const row = e.target.closest('[data-source-id]');
    if (!row || !e.target.closest('[data-source-menu]')) return;
    const sourceId = row.dataset.sourceId;
    const source = state.sources.find((s) => (s.source_id ?? s.sourceId) === sourceId);
    if (!source) return;
    switch (e.detail?.action) {
      case 'edit': openSourceEditWindow(source); break;
      case 'reingest': reingestSource(sourceId); break;
      case 'files': showFilesFor(sourceId); break;
      case 'delete': confirmDeleteSource(source); break;
      default: break;
    }
  });

  // Resume live tracking for any job the server reports as running.
  for (const source of state.sources) {
    const job = source.last_job ?? source.lastJob;
    if (job && job.status === 'running') {
      trackIngestJob(job.job_id ?? job.jobId);
    }
  }
}

function showFilesFor(sourceId) {
  state.files = { sourceId, offset: 0, filter: '', rows: [], total: 0 };
  state.kbView = 'files';
  byId('ps-kb-view')?.setAttribute('value', 'files');
  renderKnowledgeView();
}

async function reingestSource(sourceId) {
  try {
    const resp = await ApiBinary.one('projectStudioSourceReingestRequest', { projectId: projectId(), sourceId });
    toast(t('reingest_started'), 'success');
    const jobId = resp.jobId ?? resp.job_id;
    await renderSources();
    if (jobId) trackIngestJob(jobId);
  } catch (err) {
    toast(`${t('reingest_failed')}: ${err.message}`, 'error');
  }
}

async function cancelIngest(jobId) {
  try {
    await ApiBinary.one('projectStudioIngestCancelRequest', { projectId: projectId(), jobId });
    toast(t('ingest_cancel_ok'), 'success');
  } catch (err) {
    toast(`${t('ingest_cancel_failed')}: ${err.message}`, 'error');
  }
}

function confirmDeleteSource(source) {
  const sourceId = source.source_id ?? source.sourceId;
  openDeleteWindow({
    title: t('delete_source_title'),
    targetName: source.name,
    targetSub: t(`kind_${source.kind}`),
    targetIcon: SOURCE_KIND_ICON[source.kind] || 'file-text',
    warning: t('delete_source_warning'),
    items: [
      {
        icon: 'file-text',
        name: t('delete_source_item_files'),
        sub: t('delete_source_item_files_sub', {
          files: source.file_count ?? source.fileCount ?? 0,
          chunks: source.chunk_count ?? source.chunkCount ?? 0,
        }),
      },
    ],
    confirmLabel: t('delete_forever'),
    onConfirm: async () => {
      await ApiBinary.one('projectStudioSourceDeleteRequest', { projectId: projectId(), sourceId });
      toast(t('delete_source_ok'), 'success');
      await renderSources();
    },
  });
}

// ---- Live ingest tracking: stream feeds the log, 3 s poll is authoritative --

function trackIngestJob(jobId) {
  if (!jobId || state.jobs.has(jobId)) return;
  const entry = { job: null, log: [], unsub: null };
  state.jobs.set(jobId, entry);

  ApiBinary.subscribe(
    'projectStudioIngestStreamRequest',
    { projectId: projectId(), jobId },
    {
      onChunk: (body) => {
        if (body?.variant !== 'ProjectStudioIngestStreamChunk') return;
        if (!state.jobs.has(jobId)) return;
        const kind = body.kind;
        if (kind === 'log' || kind === 'phase' || kind === 'file') {
          const line = kind === 'phase' ? `[${body.phase}] ${body.line}` : body.line;
          entry.log.push(line);
          if (entry.log.length > INGEST_LOG_CAP) entry.log.splice(0, entry.log.length - INGEST_LOG_CAP);
          const logEl = document.querySelector(`[data-job-log="${CSS.escape(jobId)}"]`);
          if (logEl) {
            logEl.textContent = entry.log.join('\n');
            logEl.scrollTop = logEl.scrollHeight;
          }
        } else if (kind === 'progress') {
          updateJobProgressUi(jobId, body.progress_pct ?? body.progressPct ?? 0);
        }
      },
      onError: () => { /* the poll below is the source of truth */ },
      onEnd: () => { /* terminal state is confirmed by the poll */ },
    },
  ).then((unsub) => {
    if (!state.jobs.has(jobId)) { unsub(); return; }
    entry.unsub = unsub;
  }).catch(() => { /* stream is optional; polling still tracks the job */ });

  ensureJobsPoll();
}

function updateJobProgressUi(jobId, pct) {
  const clamped = Math.max(0, Math.min(100, Number(pct) || 0));
  document.querySelector(`[data-job-progress="${CSS.escape(jobId)}"]`)?.setAttribute('value', String(clamped));
  const pctEl = document.querySelector(`[data-job-pct="${CSS.escape(jobId)}"]`);
  if (pctEl) pctEl.textContent = `${clamped}%`;
}

function ensureJobsPoll() {
  if (state.jobsPollTimer) return;
  state.jobsPollTimer = setInterval(async () => {
    if (!state.jobs.size) {
      clearInterval(state.jobsPollTimer);
      state.jobsPollTimer = null;
      return;
    }
    let anyFinished = false;
    for (const [jobId, entry] of [...state.jobs]) {
      let job = null;
      try {
        const resp = await ApiBinary.one('projectStudioIngestStatusRequest', { projectId: projectId(), jobId });
        job = resp.job;
      } catch {
        continue;
      }
      if (!job) continue;
      entry.job = job;
      const filesTotal = job.files_total ?? job.filesTotal ?? 0;
      const filesDone = job.files_done ?? job.filesDone ?? 0;
      if (filesTotal > 0) updateJobProgressUi(jobId, Math.round((filesDone / filesTotal) * 100));
      if (job.status !== 'running') {
        if (entry.unsub) { entry.unsub(); entry.unsub = null; }
        state.jobs.delete(jobId);
        anyFinished = true;
        if (job.status === 'failed' && job.error) {
          toast(`${t('ingest_failed')}: ${job.error}`, 'error');
        } else if (job.status === 'success') {
          toast(t('ingest_done'), 'success');
        }
      }
    }
    if (anyFinished && state.tab === 'knowledge' && state.kbView === 'sources') {
      await renderSources();
    }
  }, INGEST_POLL_MS);
}

function stopAllJobTracking() {
  for (const [, entry] of state.jobs) {
    if (entry.unsub) entry.unsub();
  }
  state.jobs.clear();
  if (state.jobsPollTimer) {
    clearInterval(state.jobsPollTimer);
    state.jobsPollTimer = null;
  }
}

// =============================================================================
// W02 — add source window (documents upload + URL; other kinds announced)
// =============================================================================

function openSourceWindow() {
  const { body, foot, cleanup } = openWindow({
    title: t('source_win_title'),
    icon: 'database',
    width: 640,
  });

  const sw = {
    kind: 'document',
    // [{ file, progress (0-100 | null), ref }]
    files: [],
    busy: false,
  };

  const kindsHtml = SOURCE_KINDS.map((k) => `
    <div class="ps-choice-card ${sw.kind === k.id ? 'is-selected' : ''} ${k.enabled ? '' : 'is-disabled'}"
         data-kind="${escapeAttr(k.id)}" role="button" tabindex="${k.enabled ? '0' : '-1'}">
      <div class="ps-cc-ico">${sprite(k.icon)}</div>
      <div>
        <div class="ps-cc-name">${escapeHtml(t(`kind_${k.id}`))}${k.enabled ? '' : ` <tf-chip status="info">${escapeHtml(t('kind_soon'))}</tf-chip>`}</div>
        <div class="ps-cc-desc">${escapeHtml(t(`kind_${k.id}_desc`))}</div>
      </div>
    </div>
  `).join('');

  body.innerHTML = `
    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('source_kind_label'))}</span>
      <div class="ps-field-hint">${escapeHtml(t('source_kind_hint'))}</div>
      <div class="ps-choice-grid" data-kind-grid>${kindsHtml}</div>
    </div>
    <tf-input id="ps-src-name" label="${escapeAttr(t('source_name_label'))}"></tf-input>
    <div data-kind-form="document">
      <div class="ps-field">
        <span class="ps-field-label">${escapeHtml(t('source_files_label'))}</span>
        <tf-file-input id="ps-src-files" multiple label="${escapeAttr(t('source_dropzone'))}"></tf-file-input>
        <div class="ps-field-hint">${escapeHtml(t('source_files_hint'))}</div>
      </div>
      <div data-file-list></div>
    </div>
    <div data-kind-form="url" hidden>
      <tf-input id="ps-src-url" label="${escapeAttr(t('source_url_label'))}" placeholder="https://…"
        hint="${escapeAttr(t('source_url_hint'))}"></tf-input>
    </div>
    <div class="ps-field-hint">${escapeHtml(t('source_queue_hint'))}</div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;

  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="primary" icon="plus" data-action="save">${escapeHtml(t('source_submit'))}</tf-button>
    </div>
  `;

  const showError = (message) => {
    const el = body.querySelector('[data-form-error]');
    if (!el) return;
    el.hidden = !message;
    el.textContent = message || '';
  };

  const renderFileList = () => {
    const host = body.querySelector('[data-file-list]');
    if (!host) return;
    host.innerHTML = sw.files.map((f, i) => `
      <div class="ps-added-file">
        <span class="ps-af-ico">${sprite('file-text')}</span>
        <div class="ps-af-main">
          <div class="ps-af-name">${escapeHtml(f.file.name)}</div>
          <div class="ps-af-size">${escapeHtml(formatBytes(f.file.size))}</div>
        </div>
        ${f.progress != null
          ? `<tf-progress-bar class="ps-af-progress" size="sm" value="${f.progress}" data-upload-progress="${i}"></tf-progress-bar>`
          : `<tf-button variant="ghost" size="sm" icon="trash" data-file-remove="${i}" title="${escapeAttr(t('action_delete'))}"></tf-button>`}
      </div>
    `).join('');
  };

  body.querySelector('[data-kind-grid]')?.addEventListener('click', (e) => {
    const card = e.target.closest('[data-kind]');
    if (!card || card.classList.contains('is-disabled') || sw.busy) return;
    sw.kind = card.dataset.kind;
    body.querySelectorAll('[data-kind]').forEach((c) => {
      c.classList.toggle('is-selected', c.dataset.kind === sw.kind);
    });
    body.querySelectorAll('[data-kind-form]').forEach((form) => {
      form.hidden = form.dataset.kindForm !== sw.kind;
    });
    showError(null);
  });

  body.querySelector('#ps-src-files')?.addEventListener('change', (e) => {
    const files = e.detail?.files;
    if (!files) return;
    for (const file of Array.from(files)) {
      const dup = sw.files.some((f) => f.file.name === file.name && f.file.size === file.size);
      if (!dup) sw.files.push({ file, progress: null, ref: null });
    }
    renderFileList();
  });

  body.addEventListener('click', (e) => {
    const remove = e.target.closest('[data-file-remove]');
    if (remove && !sw.busy) {
      sw.files.splice(Number(remove.dataset.fileRemove), 1);
      renderFileList();
    }
  });

  // Uploads one file in 1 MiB chunks; the final chunk response carries the
  // content-hash file_ref used by SourceCreate.
  const uploadFile = async (item, index) => {
    const uploadId = crypto.randomUUID();
    const buffer = new Uint8Array(await item.file.arrayBuffer());
    const totalChunks = Math.max(1, Math.ceil(buffer.length / UPLOAD_CHUNK_BYTES));
    let fileRef = null;
    for (let seq = 0; seq < totalChunks; seq += 1) {
      const chunk = buffer.subarray(seq * UPLOAD_CHUNK_BYTES, Math.min((seq + 1) * UPLOAD_CHUNK_BYTES, buffer.length));
      const resp = await ApiBinary.one('projectStudioSourceUploadChunkRequest', {
        projectId: projectId(),
        uploadId,
        filename: item.file.name,
        mime: item.file.type || 'application/octet-stream',
        seq,
        totalChunks,
        bytes: chunk,
      });
      item.progress = Math.round(((seq + 1) / totalChunks) * 100);
      body.querySelector(`[data-upload-progress="${index}"]`)?.setAttribute('value', String(item.progress));
      fileRef = resp.fileRef ?? resp.file_ref ?? fileRef;
    }
    if (!fileRef) throw new Error(t('upload_no_ref'));
    return fileRef;
  };

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || sw.busy) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }

    const name = String(body.querySelector('#ps-src-name')?.value ?? '').trim();
    if (!name) { showError(t('err_source_name')); return; }

    let configJson = '{}';
    if (sw.kind === 'document') {
      if (!sw.files.length) { showError(t('err_source_files')); return; }
    } else if (sw.kind === 'url') {
      const url = String(body.querySelector('#ps-src-url')?.value ?? '').trim();
      if (!/^https:\/\/.+/.test(url)) { showError(t('err_source_url')); return; }
      configJson = JSON.stringify({ url });
    }

    showError(null);
    sw.busy = true;
    btn.setAttribute('disabled', '');
    try {
      const fileRefs = [];
      if (sw.kind === 'document') {
        for (let i = 0; i < sw.files.length; i += 1) {
          sw.files[i].progress = 0;
          renderFileList();
          fileRefs.push(await uploadFile(sw.files[i], i));
        }
      }
      const resp = await ApiBinary.one('projectStudioSourceCreateRequest', {
        projectId: projectId(),
        kind: sw.kind,
        name,
        configJson,
        fileRefs,
      });
      toast(t('source_created'), 'success');
      cleanup();
      const jobId = resp.jobId ?? resp.job_id;
      if (state.tab === 'knowledge' && state.kbView === 'sources') await renderSources();
      if (jobId) trackIngestJob(jobId);
    } catch (err) {
      sw.busy = false;
      btn.removeAttribute('disabled');
      sw.files.forEach((f) => { f.progress = null; });
      renderFileList();
      showError(`${t('source_create_failed')}: ${err.message}`);
    }
  });
}

function openSourceEditWindow(source) {
  const sourceId = source.source_id ?? source.sourceId;
  const { body, foot, cleanup } = openWindow({
    title: t('source_edit_title'),
    subtitle: source.name,
    icon: 'edit',
    width: 520,
  });

  let config = {};
  try { config = JSON.parse(source.config_json ?? source.configJson ?? '{}') || {}; } catch { config = {}; }

  body.innerHTML = `
    <tf-input id="ps-src-edit-name" label="${escapeAttr(t('source_name_label'))}" value="${escapeAttr(source.name)}"></tf-input>
    ${source.kind === 'url' ? `
      <tf-input id="ps-src-edit-url" label="${escapeAttr(t('source_url_label'))}" value="${escapeAttr(config.url || '')}"
        hint="${escapeAttr(t('source_url_hint'))}"></tf-input>
    ` : ''}
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="primary" icon="check" data-action="save">${escapeHtml(t('action_save'))}</tf-button>
    </div>
  `;

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    const name = String(body.querySelector('#ps-src-edit-name')?.value ?? '').trim();
    const errEl = body.querySelector('[data-form-error]');
    if (!name) {
      if (errEl) { errEl.hidden = false; errEl.textContent = t('err_source_name'); }
      return;
    }
    let configJson = source.config_json ?? source.configJson ?? '{}';
    if (source.kind === 'url') {
      const url = String(body.querySelector('#ps-src-edit-url')?.value ?? '').trim();
      if (!/^https:\/\/.+/.test(url)) {
        if (errEl) { errEl.hidden = false; errEl.textContent = t('err_source_url'); }
        return;
      }
      configJson = JSON.stringify({ ...config, url });
    }
    try {
      const resp = await ApiBinary.one('projectStudioSourceUpdateRequest', {
        projectId: projectId(), sourceId, name, configJson,
      });
      toast(t('source_saved'), 'success');
      cleanup();
      await renderSources();
      const jobId = resp.jobId ?? resp.job_id;
      if (jobId) trackIngestJob(jobId);
    } catch (err) {
      if (errEl) { errEl.hidden = false; errEl.textContent = `${t('source_save_failed')}: ${err.message}`; }
    }
  });
}

// =============================================================================
// W03 — knowledge-base search
// =============================================================================

function renderKbSearch() {
  const host = byId('ps-kb-host');
  if (!host) return;

  host.innerHTML = `
    <tf-section-card title="${escapeAttr(t('kb_search_title'))}" icon="search">
      <div class="ps-kb-bar">
        <tf-searchbox id="ps-kb-query" placeholder="${escapeAttr(t('kb_query_placeholder'))}" value="${escapeAttr(state.kbQuery)}"></tf-searchbox>
        <tf-button variant="primary" icon="search" id="ps-kb-run">${escapeHtml(t('kb_search_btn'))}</tf-button>
      </div>
      <div class="ps-kb-filters">
        <span class="ps-kb-filters-label">${sprite('filter')}${escapeHtml(t('kb_filters_label'))}</span>
        <tf-filter-chips id="ps-kb-sources-filter" mode="multi"></tf-filter-chips>
      </div>
      <div id="ps-kb-results"></div>
    </tf-section-card>
  `;

  const chips = byId('ps-kb-sources-filter');
  if (chips) {
    chips.filters = state.sources.map((s) => ({
      id: s.source_id ?? s.sourceId,
      label: s.name,
      active: state.kbSelectedSources.has(s.source_id ?? s.sourceId),
    }));
    chips.addEventListener('change', (e) => {
      const id = e.detail?.id;
      if (!id) return;
      if (e.detail.active) state.kbSelectedSources.add(id);
      else state.kbSelectedSources.delete(id);
    });
  }

  byId('ps-kb-run')?.addEventListener('click', () => runKbSearch());
  byId('ps-kb-query')?.addEventListener('search', (e) => {
    state.kbQuery = String(e.detail?.value ?? '');
  });
  byId('ps-kb-query')?.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') runKbSearch();
  });

  renderKbResults();
  // Source chips need the source list — lazy-load it when the operator lands
  // straight on the search view.
  if (!state.sources.length) {
    loadSources().then(() => {
      if (state.kbView !== 'search') return;
      const chipHost = byId('ps-kb-sources-filter');
      if (chipHost) {
        chipHost.filters = state.sources.map((s) => ({
          id: s.source_id ?? s.sourceId,
          label: s.name,
          active: state.kbSelectedSources.has(s.source_id ?? s.sourceId),
        }));
      }
    }).catch(() => { /* chips just stay empty */ });
  }
}

async function runKbSearch() {
  const query = String(byId('ps-kb-query')?.value ?? state.kbQuery ?? '').trim();
  if (!query) return;
  state.kbQuery = query;
  state.kbSearching = true;
  renderKbResults();
  try {
    const resp = await ApiBinary.one('projectStudioKbSearchRequest', {
      projectId: projectId(),
      query,
      sourceIds: [...state.kbSelectedSources],
      limit: 20,
    });
    state.kbHits = Array.isArray(resp.hits) ? resp.hits : [];
  } catch (err) {
    state.kbHits = null;
    toast(`${t('kb_search_failed')}: ${err.message}`, 'error');
  } finally {
    state.kbSearching = false;
    renderKbResults();
  }
}

function kbHitHtml(hit) {
  const score = Number(hit.score ?? 0);
  return `
    <div class="ps-kb-result">
      <div class="ps-kb-head">
        <div class="ps-kb-ico">${sprite(SOURCE_KIND_ICON[hit.source_kind ?? hit.sourceKind] || 'file-text')}</div>
        <div>
          <div class="ps-kb-title">${escapeHtml(hit.source_name ?? hit.sourceName ?? '')}</div>
          <div class="ps-kb-path">${escapeHtml(hit.file_path ?? hit.filePath ?? '')} · ${escapeHtml(hit.location || '')} · #${hit.chunk_index ?? hit.chunkIndex ?? 0}</div>
        </div>
        <tf-chip class="ps-kb-score" status="accent">${escapeHtml(score.toFixed(2))}</tf-chip>
      </div>
      <div class="ps-kb-snippet">${escapeHtml(hit.snippet || '')}</div>
      <div class="ps-kb-foot">
        <tf-button variant="ghost" size="sm" icon="eye" data-kb-preview="${escapeAttr(hit.file_id ?? hit.fileId)}">${escapeHtml(t('kb_preview'))}</tf-button>
        <tf-button variant="ghost" size="sm" icon="file-text" data-kb-open-file="${escapeAttr(hit.source_id ?? hit.sourceId)}">${escapeHtml(t('kb_open_in_files'))}</tf-button>
      </div>
    </div>
  `;
}

function renderKbResults() {
  const host = byId('ps-kb-results');
  if (!host) return;
  if (state.kbSearching) {
    host.innerHTML = `<div class="ps-loading"><tf-spinner size="sm"></tf-spinner> ${escapeHtml(t('kb_searching'))}</div>`;
    return;
  }
  if (state.kbHits === null) {
    host.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('kb_intro'))}</div>`;
    return;
  }
  if (!state.kbHits.length) {
    host.innerHTML = `<tf-empty-state icon="search" title="${escapeAttr(t('kb_no_results', { query: state.kbQuery }))}"></tf-empty-state>`;
    return;
  }
  host.innerHTML = `
    <div class="ps-kb-meta">${escapeHtml(t('kb_results_meta', { count: state.kbHits.length }))}</div>
    ${state.kbHits.map(kbHitHtml).join('')}
  `;
  host.querySelectorAll('[data-kb-preview]').forEach((btn) => {
    btn.addEventListener('click', () => openFilePreview(btn.dataset.kbPreview));
  });
  host.querySelectorAll('[data-kb-open-file]').forEach((btn) => {
    btn.addEventListener('click', () => showFilesFor(btn.dataset.kbOpenFile));
  });
}

// =============================================================================
// W04 — source files with pagination + preview
// =============================================================================

async function renderFilesView() {
  const host = byId('ps-kb-host');
  if (!host) return;
  if (!state.sources.length) {
    try { await loadSources(); } catch { /* select stays empty */ }
  }
  if (!state.files.sourceId && state.sources.length) {
    state.files.sourceId = state.sources[0].source_id ?? state.sources[0].sourceId;
  }

  host.innerHTML = `
    <tf-section-card title="${escapeAttr(t('files_title'))}" icon="file-text">
      <div class="ps-files-toolbar">
        <tf-select id="ps-files-source" value="${escapeAttr(state.files.sourceId || '')}">
          ${state.sources.map((s) => {
            const id = s.source_id ?? s.sourceId;
            return `<option value="${escapeAttr(id)}" ${id === state.files.sourceId ? 'selected' : ''}>${escapeHtml(s.name)}</option>`;
          }).join('')}
        </tf-select>
        <tf-searchbox id="ps-files-filter" placeholder="${escapeAttr(t('files_filter_placeholder'))}" debounce="300" value="${escapeAttr(state.files.filter)}"></tf-searchbox>
      </div>
      <div id="ps-files-table-host"></div>
      <div class="ps-files-pager" id="ps-files-pager" hidden>
        <span id="ps-files-range"></span>
        <tf-button variant="ghost" size="sm" icon="chevron-left" id="ps-files-prev"></tf-button>
        <tf-button variant="ghost" size="sm" icon="chevron-right" id="ps-files-next"></tf-button>
      </div>
    </tf-section-card>
  `;

  byId('ps-files-source')?.addEventListener('change', (e) => {
    state.files.sourceId = e.detail?.value ?? e.target.value;
    state.files.offset = 0;
    loadFilesPage();
  });
  byId('ps-files-filter')?.addEventListener('search', (e) => {
    state.files.filter = String(e.detail?.value ?? '');
    state.files.offset = 0;
    loadFilesPage();
  });
  byId('ps-files-prev')?.addEventListener('click', () => {
    if (state.files.offset <= 0) return;
    state.files.offset = Math.max(0, state.files.offset - FILES_PAGE_SIZE);
    loadFilesPage();
  });
  byId('ps-files-next')?.addEventListener('click', () => {
    if (state.files.offset + FILES_PAGE_SIZE >= state.files.total) return;
    state.files.offset += FILES_PAGE_SIZE;
    loadFilesPage();
  });

  if (state.files.sourceId) await loadFilesPage();
  else byId('ps-files-table-host').innerHTML = `<tf-empty-state icon="file-text" title="${escapeAttr(t('files_no_source'))}"></tf-empty-state>`;
}

async function loadFilesPage() {
  const tableHost = byId('ps-files-table-host');
  if (!tableHost) return;
  tableHost.innerHTML = `<div class="ps-loading">${escapeHtml(t('loading'))}</div>`;
  try {
    const resp = await ApiBinary.one('projectStudioSourceFilesListRequest', {
      projectId: projectId(),
      sourceId: state.files.sourceId,
      offset: state.files.offset,
      limit: FILES_PAGE_SIZE,
      filter: state.files.filter,
    });
    state.files.rows = Array.isArray(resp.files) ? resp.files : [];
    state.files.total = Number(resp.total ?? state.files.rows.length);
  } catch (err) {
    tableHost.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('files_failed')}: ${err.message}`)}</div>`;
    return;
  }
  renderFilesTable();
}

function renderFilesTable() {
  const tableHost = byId('ps-files-table-host');
  if (!tableHost) return;
  if (!state.files.rows.length) {
    tableHost.innerHTML = `<tf-empty-state icon="file-text" title="${escapeAttr(t('files_empty'))}"></tf-empty-state>`;
    const emptyPager = byId('ps-files-pager');
    if (emptyPager) emptyPager.hidden = true;
    return;
  }
  tableHost.innerHTML = `
    <tf-table id="ps-files-table">
      <tf-column key="path" label="${escapeAttr(t('files_col_path'))}"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('files_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="note" label="${escapeAttr(t('files_col_note'))}"></tf-column>
      <tf-column key="size" label="${escapeAttr(t('files_col_size'))}"></tf-column>
      <tf-column key="chunks" label="${escapeAttr(t('files_col_chunks'))}" renderer="num"></tf-column>
      <tf-column key="updated" label="${escapeAttr(t('files_col_updated'))}"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-files-table');
  table.rows = state.files.rows.map((f) => ({
    _id: f.file_id ?? f.fileId,
    _status: f.status,
    path: f.path,
    status: { status: FILE_STATUS_CHIP[f.status] || 'info', label: t(`file_status_${f.status}`) },
    // Skip/error reason surfaces inline (skipped files carry it in `error`).
    note: f.error || '—',
    size: formatBytes(Number(f.size_bytes ?? f.sizeBytes ?? 0)),
    chunks: f.chunk_count ?? f.chunkCount ?? 0,
    updated: formatTimestamp(f.updated_at ?? f.updatedAt),
  }));
  table.rowActions = (row) => {
    const wrap = document.createElement('div');
    wrap.className = 'ps-file-actions';
    const previewBtn = document.createElement('tf-button');
    previewBtn.setAttribute('variant', 'ghost');
    previewBtn.setAttribute('size', 'sm');
    previewBtn.setAttribute('icon', 'eye');
    previewBtn.setAttribute('title', t('kb_preview'));
    previewBtn.addEventListener('click', (e) => { e.stopPropagation(); openFilePreview(row._id); });
    wrap.appendChild(previewBtn);
    if (canEdit()) {
      const delBtn = document.createElement('tf-button');
      delBtn.setAttribute('variant', 'ghost');
      delBtn.setAttribute('size', 'sm');
      delBtn.setAttribute('icon', 'trash');
      delBtn.setAttribute('title', t('action_delete'));
      delBtn.addEventListener('click', (e) => { e.stopPropagation(); confirmDeleteFile(row); });
      wrap.appendChild(delBtn);
    }
    return wrap;
  };
  table.addEventListener('row-click', (e) => {
    const id = e.detail?.row?._id;
    if (id) openFilePreview(id);
  });

  const pager = byId('ps-files-pager');
  if (pager) {
    pager.hidden = state.files.total <= FILES_PAGE_SIZE;
    const from = state.files.offset + 1;
    const to = Math.min(state.files.offset + state.files.rows.length, state.files.total);
    const range = byId('ps-files-range');
    if (range) range.textContent = t('files_range', { from, to, total: state.files.total });
  }
}

function confirmDeleteFile(row) {
  openDeleteWindow({
    title: t('delete_file_title'),
    targetName: row.path,
    targetSub: row.size,
    targetIcon: 'file-text',
    warning: t('delete_file_warning'),
    confirmLabel: t('delete_forever'),
    onConfirm: async () => {
      await ApiBinary.one('projectStudioSourceFileDeleteRequest', { projectId: projectId(), fileId: row._id });
      toast(t('delete_file_ok'), 'success');
      await loadFilesPage();
    },
  });
}

async function openFilePreview(fileId) {
  let preview = null;
  try {
    preview = await ApiBinary.one('projectStudioSourceFilePreviewRequest', {
      projectId: projectId(), fileId, maxBytes: 262144,
    });
  } catch (err) {
    toast(`${t('preview_failed')}: ${err.message}`, 'error');
    return;
  }
  const { body, foot, cleanup } = openWindow({
    title: t('preview_title'),
    subtitle: preview.mime || '',
    icon: 'eye',
    width: 820,
  });
  const content = String(preview.content ?? '');
  const lines = content.split('\n');
  const gutter = lines.map((_, i) => i + 1).join('\n');
  body.innerHTML = `
    ${(preview.truncated ?? false) ? `<div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('preview_truncated'))}</span></div>` : ''}
    <div class="ps-preview-body">
      <div class="ps-preview-gutter">${gutter}</div>
      <pre class="ps-preview-code">${escapeHtml(content)}</pre>
    </div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="close-preview">${escapeHtml(t('action_close'))}</tf-button>
    </div>
  `;
  foot.addEventListener('click', (e) => {
    if (e.target.closest('[data-action="close-preview"]')) cleanup();
  });
}

// =============================================================================
// C01 — project chat (private per user, streamed replies with citations)
// =============================================================================

async function renderChat() {
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  try {
    const resp = await ApiBinary.one('projectStudioChatsListRequest', { projectId: projectId() });
    state.chats = Array.isArray(resp.chats) ? resp.chats : [];
  } catch (err) {
    panel.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('chat_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'chat') return;
  if (state.chatId && !state.chats.some((c) => (c.chat_id ?? c.chatId) === state.chatId)) {
    state.chatId = null;
  }

  panel.innerHTML = `
    <div class="ps-chat-layout">
      <div class="ps-chat-list">
        <tf-button variant="primary" size="sm" icon="plus" id="ps-chat-new">${escapeHtml(t('chat_new'))}</tf-button>
        <div class="ps-chat-private-hint">${sprite('eye')}${escapeHtml(t('chat_private_hint'))}</div>
        <div id="ps-chat-convs"></div>
      </div>
      <div class="ps-chat-main">
        <div class="ps-chat-head">
          <div>
            <div class="ps-chat-title" id="ps-chat-title"></div>
            <div class="ps-chat-sub">${escapeHtml(t('chat_context_sub'))}</div>
          </div>
          <div class="ps-chat-head-actions">
            <tf-button variant="ghost" size="sm" icon="edit" id="ps-chat-rename" hidden>${escapeHtml(t('chat_rename'))}</tf-button>
            <tf-button variant="ghost" size="sm" icon="trash" id="ps-chat-delete" hidden title="${escapeAttr(t('chat_delete'))}"></tf-button>
          </div>
        </div>
        <div class="ps-chat-msgs" id="ps-chat-msgs"></div>
        <div class="ps-chat-notice">${sprite('shield')}<span>${escapeHtml(t('chat_notice'))}</span></div>
        <tf-chat-composer id="ps-chat-composer" placeholder="${escapeAttr(t('chat_placeholder'))}"></tf-chat-composer>
      </div>
    </div>
  `;

  byId('ps-chat-new')?.addEventListener('click', () => {
    stopChatStream();
    state.chatId = null;
    state.chatMessages = [];
    renderChatConvs();
    renderChatHeader();
    renderChatMessages();
  });
  byId('ps-chat-rename')?.addEventListener('click', () => renameCurrentChat());
  byId('ps-chat-delete')?.addEventListener('click', () => deleteCurrentChat());
  byId('ps-chat-composer')?.addEventListener('send', (e) => {
    const text = String(e.detail?.text ?? '').trim();
    if (text) sendChatMessage(text);
  });
  const convs = byId('ps-chat-convs');
  convs?.addEventListener('click', (e) => {
    const rename = e.target.closest('[data-conv-rename]');
    if (rename) {
      e.stopPropagation();
      renameChat(rename.dataset.convRename);
      return;
    }
    const del = e.target.closest('[data-conv-delete]');
    if (del) {
      e.stopPropagation();
      deleteChat(del.dataset.convDelete);
      return;
    }
    const conv = e.target.closest('[data-conv]');
    if (conv) selectChat(conv.dataset.conv);
  });

  renderChatConvs();
  renderChatHeader();
  if (state.chatId) await loadChatHistory();
  else renderChatMessages();
}

function renderChatConvs() {
  const host = byId('ps-chat-convs');
  if (!host) return;
  if (!state.chats.length) {
    host.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('chat_empty_list'))}</div>`;
    return;
  }
  host.innerHTML = state.chats.map((c) => {
    const chatId = c.chat_id ?? c.chatId;
    return `
      <div class="ps-chat-conv ${chatId === state.chatId ? 'is-active' : ''}" data-conv="${escapeAttr(chatId)}" role="button" tabindex="0">
        <div class="ps-conv-main">
          <div class="ps-conv-title">${escapeHtml(c.title || t('chat_untitled'))}</div>
          <div class="ps-conv-sub">${escapeHtml(c.last_message_preview ?? c.lastMessagePreview ?? '')}</div>
        </div>
        <div class="ps-conv-actions">
          <tf-button variant="ghost" size="sm" icon="edit" data-conv-rename="${escapeAttr(chatId)}" title="${escapeAttr(t('chat_rename'))}"></tf-button>
          <tf-button variant="ghost" size="sm" icon="trash" data-conv-delete="${escapeAttr(chatId)}" title="${escapeAttr(t('chat_delete'))}"></tf-button>
        </div>
      </div>
    `;
  }).join('');
}

function renderChatHeader() {
  const title = byId('ps-chat-title');
  const chat = state.chats.find((c) => (c.chat_id ?? c.chatId) === state.chatId);
  if (title) title.textContent = chat ? (chat.title || t('chat_untitled')) : t('chat_new_conversation');
  const renameBtn = byId('ps-chat-rename');
  const deleteBtn = byId('ps-chat-delete');
  if (renameBtn) renameBtn.hidden = !chat;
  if (deleteBtn) deleteBtn.hidden = !chat;
}

async function selectChat(chatId) {
  if (state.chatBusy) stopChatStream();
  state.chatId = chatId;
  state.chatMessages = [];
  renderChatConvs();
  renderChatHeader();
  await loadChatHistory();
}

async function loadChatHistory() {
  try {
    const resp = await ApiBinary.one('projectStudioChatHistoryRequest', {
      projectId: projectId(), chatId: state.chatId, limit: 50,
    });
    const messages = Array.isArray(resp.messages) ? resp.messages : [];
    // The wire pages newest-first; the transcript renders oldest-first.
    state.chatMessages = messages.slice().reverse().map((m) => ({
      role: m.role,
      content: m.content,
      citationsJson: m.citations_json ?? m.citationsJson ?? '',
      time: formatTimestamp(m.created_at ?? m.createdAt),
      streaming: false,
    }));
  } catch (err) {
    toast(`${t('chat_history_failed')}: ${err.message}`, 'error');
    state.chatMessages = [];
  }
  renderChatMessages();
}

function parseCitations(citationsJson) {
  try {
    const arr = JSON.parse(citationsJson || '[]');
    return Array.isArray(arr) ? arr : [];
  } catch {
    return [];
  }
}

function citationsHtml(citationsJson) {
  const citations = parseCitations(citationsJson);
  if (!citations.length) return '';
  const rows = citations.map((c, i) => {
    const title = c.source_name ?? c.sourceName ?? c.title ?? c.file_path ?? c.filePath ?? t('chat_source');
    const path = c.file_path ?? c.filePath ?? c.location ?? '';
    const snippet = c.snippet ?? c.text ?? '';
    return `
      <div class="ps-chat-src">
        <div class="ps-chat-src-num">${i + 1}</div>
        <div>
          <div class="ps-chat-src-file">${escapeHtml(String(title))}</div>
          ${path ? `<div class="ps-chat-src-path">${escapeHtml(String(path))}</div>` : ''}
          ${snippet ? `<div class="ps-chat-src-snip">${escapeHtml(String(snippet))}</div>` : ''}
        </div>
      </div>
    `;
  }).join('');
  return `
    <div class="ps-chat-sources">
      <div class="ps-chat-sources-head">${sprite('database')}${escapeHtml(t('chat_sources', { count: citations.length }))}</div>
      ${rows}
    </div>
  `;
}

function renderChatMessages() {
  const host = byId('ps-chat-msgs');
  if (!host) return;
  if (!state.chatMessages.length) {
    host.innerHTML = `<tf-empty-state icon="message" title="${escapeAttr(t('chat_empty_thread'))}"></tf-empty-state>`;
  } else {
    host.innerHTML = state.chatMessages.map((m) => `
      <tf-chat-bubble role="${m.role === 'user' ? 'user' : 'assistant'}"
        ${m.streaming ? 'streaming' : ''}
        sender="${escapeAttr(m.role === 'user' ? '' : t('chat_assistant'))}"
        time="${escapeAttr(m.time || '')}">${escapeHtml(m.content)}</tf-chat-bubble>
      ${m.role !== 'user' && m.citationsJson ? citationsHtml(m.citationsJson) : ''}
    `).join('');
  }
  host.scrollTop = host.scrollHeight;
  const composer = byId('ps-chat-composer');
  if (composer) {
    if (state.chatBusy) composer.setAttribute('disabled', '');
    else composer.removeAttribute('disabled');
  }
}

function nowTime() {
  return new Date().toLocaleTimeString(I18n.getLanguage(), { hour: '2-digit', minute: '2-digit' });
}

async function sendChatMessage(text) {
  if (state.chatBusy) return;

  // First message of a fresh thread creates the chat with a derived title.
  if (!state.chatId) {
    try {
      const resp = await ApiBinary.one('projectStudioChatCreateRequest', {
        projectId: projectId(),
        title: text.length > 60 ? `${text.slice(0, 57)}…` : text,
      });
      const chat = resp.chat;
      if (chat) {
        state.chats.unshift(chat);
        state.chatId = chat.chat_id ?? chat.chatId;
      }
    } catch (err) {
      toast(`${t('chat_create_failed')}: ${err.message}`, 'error');
      return;
    }
    renderChatConvs();
    renderChatHeader();
  }

  const chatId = state.chatId;
  state.chatBusy = true;
  state.chatMessages.push({ role: 'user', content: text, citationsJson: '', time: nowTime(), streaming: false });
  const assistant = { role: 'assistant', content: '', citationsJson: '', time: nowTime(), streaming: true };
  state.chatMessages.push(assistant);
  renderChatMessages();

  const fail = (message) => {
    if (!state.chatBusy) return;
    state.chatBusy = false;
    state.chatUnsub = null;
    assistant.streaming = false;
    assistant.content = assistant.content || t('chat_stream_error');
    toast(`${t('chat_stream_failed')}${message ? `: ${message}` : ''}`, 'error');
    renderChatMessages();
  };

  try {
    state.chatUnsub = await ApiBinary.subscribe(
      'projectStudioChatStreamRequest',
      { projectId: projectId(), chatId, message: text },
      {
        onChunk: (body) => {
          if (body?.variant !== 'ProjectStudioChatStreamChunk') return;
          if (state.chatId !== chatId) return;
          if (body.kind === 'token') {
            assistant.content += body.text ?? '';
            renderChatMessages();
          } else if (body.kind === 'citations') {
            assistant.citationsJson = body.citations_json ?? body.citationsJson ?? '';
            renderChatMessages();
          }
          // kind === 'status' is informational only.
        },
        onError: (body) => fail(body?.message ?? ''),
        onEnd: (body) => {
          if (state.chatId !== chatId) return;
          if (body && (body.error || body.status === 'error')) {
            fail(body.error ?? '');
            return;
          }
          state.chatBusy = false;
          state.chatUnsub = null;
          assistant.streaming = false;
          renderChatMessages();
          // Refresh previews/ordering in the conversation list.
          ApiBinary.one('projectStudioChatsListRequest', { projectId: projectId() })
            .then((resp) => {
              if (state.tab !== 'chat') return;
              state.chats = Array.isArray(resp.chats) ? resp.chats : state.chats;
              renderChatConvs();
            })
            .catch(() => { /* list refresh is cosmetic */ });
        },
      },
    );
  } catch (err) {
    fail(err.message);
  }
}

function stopChatStream() {
  if (state.chatUnsub) {
    state.chatUnsub();
    state.chatUnsub = null;
  }
  state.chatBusy = false;
  const last = state.chatMessages[state.chatMessages.length - 1];
  if (last?.streaming) last.streaming = false;
}

async function renameChat(chatId) {
  const chat = state.chats.find((c) => (c.chat_id ?? c.chatId) === chatId);
  if (!chat) return;
  const title = await openPromptWindow({
    title: t('chat_rename'),
    label: t('chat_rename_label'),
    value: chat.title || '',
  });
  if (title == null || !title) return;
  try {
    await ApiBinary.one('projectStudioChatRenameRequest', { projectId: projectId(), chatId, title });
    chat.title = title;
    renderChatConvs();
    renderChatHeader();
  } catch (err) {
    toast(`${t('chat_rename_failed')}: ${err.message}`, 'error');
  }
}

function renameCurrentChat() {
  if (state.chatId) renameChat(state.chatId);
}

async function deleteChat(chatId) {
  const chat = state.chats.find((c) => (c.chat_id ?? c.chatId) === chatId);
  if (!chat) return;
  const ok = await TfWindow.confirm({
    title: t('chat_delete_title'),
    message: t('chat_delete_message', { title: escapeHtml(chat.title || t('chat_untitled')) }),
    confirmLabel: t('action_delete'),
    cancelLabel: t('action_cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await ApiBinary.one('projectStudioChatDeleteRequest', { projectId: projectId(), chatId });
    state.chats = state.chats.filter((c) => (c.chat_id ?? c.chatId) !== chatId);
    if (state.chatId === chatId) {
      stopChatStream();
      state.chatId = null;
      state.chatMessages = [];
      renderChatMessages();
    }
    renderChatConvs();
    renderChatHeader();
  } catch (err) {
    toast(`${t('chat_delete_failed')}: ${err.message}`, 'error');
  }
}

function deleteCurrentChat() {
  if (state.chatId) deleteChat(state.chatId);
}

// =============================================================================
// X03 — members
// =============================================================================

async function renderMembers() {
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  try {
    const resp = await ApiBinary.one('projectStudioMembersListRequest', { projectId: projectId() });
    state.members = Array.isArray(resp.members) ? resp.members : [];
  } catch (err) {
    panel.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('members_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'members') return;

  panel.innerHTML = `
    <div class="ps-banner-info">${sprite('shield')}<span>${escapeHtml(t('members_banner'))}</span></div>
    <div class="ps-members-toolbar">
      <tf-searchbox id="ps-members-search" placeholder="${escapeAttr(t('members_search_placeholder'))}" debounce="200"></tf-searchbox>
      <tf-select id="ps-members-role-filter" value="${escapeAttr(state.memberRoleFilter)}">
        <option value="all">${escapeHtml(t('members_filter_all'))}</option>
        <option value="owner">${escapeHtml(roleLabel('owner'))}</option>
        ${ASSIGNABLE_ROLES.map((r) => `<option value="${r}">${escapeHtml(roleLabel(r))}</option>`).join('')}
      </tf-select>
      <span class="ps-toolbar-spacer"></span>
      ${canManage() ? `<tf-button variant="primary" icon="plus" id="ps-invite">${escapeHtml(t('members_invite'))}</tf-button>` : ''}
    </div>
    <tf-section-card title="${escapeAttr(t('members_title'))}" icon="users">
      <span slot="subtitle">${escapeHtml(t('members_count', { count: state.members.length }))}</span>
      <div id="ps-members-list"></div>
    </tf-section-card>
    <tf-section-card title="${escapeAttr(t('roles_legend_title'))}" icon="shield">
      <div class="ps-role-legend">
        ${['owner', 'manager', 'editor', 'tester', 'viewer'].map((r) => `
          <div class="ps-rl-item">
            <div class="ps-rl-top"><tf-chip status="${r === 'owner' ? 'accent' : 'info'}">${escapeHtml(roleLabel(r))}</tf-chip></div>
            <div class="ps-rl-desc">${escapeHtml(t(`role_${r}_desc`))}</div>
          </div>
        `).join('')}
      </div>
    </tf-section-card>
  `;

  byId('ps-members-search')?.addEventListener('search', (e) => {
    state.memberQuery = String(e.detail?.value ?? '');
    renderMembersList();
  });
  byId('ps-members-role-filter')?.addEventListener('change', (e) => {
    state.memberRoleFilter = e.detail?.value ?? e.target.value ?? 'all';
    renderMembersList();
  });
  byId('ps-invite')?.addEventListener('click', () => openInviteWindow());

  renderMembersList();
}

function renderMembersList() {
  const host = byId('ps-members-list');
  if (!host) return;
  const query = state.memberQuery.trim().toLowerCase();
  const visible = state.members.filter((m) => {
    if (state.memberRoleFilter !== 'all' && m.role !== state.memberRoleFilter) return false;
    if (query) {
      const haystack = `${m.display_name ?? m.displayName ?? ''} ${m.email || ''}`.toLowerCase();
      if (!haystack.includes(query)) return false;
    }
    return true;
  });
  if (!visible.length) {
    host.innerHTML = `<tf-empty-state icon="users" title="${escapeAttr(t('members_empty'))}"></tf-empty-state>`;
    return;
  }

  host.innerHTML = visible.map((m) => {
    const userId = m.user_id ?? m.userId;
    const displayName = m.display_name ?? m.displayName ?? '';
    const self = isMe(userId);
    const memberIsOwner = m.role === 'owner';
    // Managers reassign non-owner roles; ownership moves only via transfer.
    const canChangeRole = canManage() && !memberIsOwner;
    const canRemove = canManage() && !memberIsOwner && !self;
    const canTransfer = isOwner() && !memberIsOwner && !self;

    const roleCell = memberIsOwner
      ? `<tf-chip status="accent">${sprite('key')} ${escapeHtml(roleLabel('owner'))}</tf-chip>`
      : canChangeRole
        ? `<tf-select class="ps-member-role" data-role-user="${escapeAttr(userId)}" value="${escapeAttr(m.role)}">
            ${ASSIGNABLE_ROLES.map((r) => `<option value="${r}" ${r === m.role ? 'selected' : ''}>${escapeHtml(roleLabel(r))}</option>`).join('')}
          </tf-select>`
        : `<tf-chip status="info">${escapeHtml(roleLabel(m.role))}</tf-chip>`;

    return `
      <div class="ps-member-row" data-member="${escapeAttr(userId)}">
        <div class="ps-av-mini">${escapeHtml(initials(displayName))}</div>
        <div class="ps-member-main">
          <div class="ps-member-name">${escapeHtml(displayName)} ${self ? `<tf-chip status="accent">${escapeHtml(t('you_chip'))}</tf-chip>` : ''}</div>
          <div class="ps-member-mail">${escapeHtml(m.email || '')}</div>
        </div>
        <div class="ps-member-invited">
          ${escapeHtml(m.invited_by_name ?? m.invitedByName ?? '')}<br>
          <span>${escapeHtml(formatTimestamp(m.created_at ?? m.createdAt))}</span>
        </div>
        ${roleCell}
        <div class="ps-member-actions">
          ${canTransfer ? `<tf-button variant="ghost" size="sm" icon="key" data-transfer="${escapeAttr(userId)}" title="${escapeAttr(t('members_transfer'))}"></tf-button>` : ''}
          ${canRemove ? `<tf-button variant="ghost" size="sm" icon="trash" data-remove-member="${escapeAttr(userId)}" title="${escapeAttr(t('members_remove'))}"></tf-button>` : ''}
        </div>
      </div>
    `;
  }).join('');

  host.querySelectorAll('[data-role-user]').forEach((sel) => {
    sel.addEventListener('change', async (e) => {
      const userId = sel.dataset.roleUser;
      const role = e.detail?.value ?? sel.value;
      try {
        await ApiBinary.one('projectStudioMemberRoleSetRequest', { projectId: projectId(), userId, role });
        const member = state.members.find((m) => (m.user_id ?? m.userId) === userId);
        if (member) member.role = role;
        toast(t('members_role_ok'), 'success');
      } catch (err) {
        toast(`${t('members_role_failed')}: ${err.message}`, 'error');
        renderMembersList();
      }
    });
  });
  host.querySelectorAll('[data-remove-member]').forEach((btn) => {
    btn.addEventListener('click', () => removeMember(btn.dataset.removeMember));
  });
  host.querySelectorAll('[data-transfer]').forEach((btn) => {
    btn.addEventListener('click', () => transferOwnership(btn.dataset.transfer));
  });
}

async function removeMember(userId) {
  const member = state.members.find((m) => (m.user_id ?? m.userId) === userId);
  if (!member) return;
  const ok = await TfWindow.confirm({
    title: t('members_remove_title'),
    message: t('members_remove_message', { name: escapeHtml(member.display_name ?? member.displayName ?? '') }),
    confirmLabel: t('members_remove'),
    cancelLabel: t('action_cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await ApiBinary.one('projectStudioMemberRemoveRequest', { projectId: projectId(), userId });
    toast(t('members_remove_ok'), 'success');
    await renderMembers();
  } catch (err) {
    toast(`${t('members_remove_failed')}: ${err.message}`, 'error');
  }
}

async function transferOwnership(userId) {
  const member = state.members.find((m) => (m.user_id ?? m.userId) === userId);
  if (!member) return;
  const ok = await TfWindow.confirm({
    title: t('members_transfer_title'),
    message: t('members_transfer_message', { name: escapeHtml(member.display_name ?? member.displayName ?? '') }),
    confirmLabel: t('members_transfer'),
    cancelLabel: t('action_cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await ApiBinary.one('projectStudioOwnershipTransferRequest', { projectId: projectId(), newOwnerUserId: userId });
    toast(t('members_transfer_ok'), 'success');
    await refreshProjectHeader();
  } catch (err) {
    toast(`${t('members_transfer_failed')}: ${err.message}`, 'error');
  }
}

function openInviteWindow() {
  const { body, foot, cleanup } = openWindow({
    title: t('invite_title'),
    icon: 'users',
    width: 560,
  });

  const inv = {
    // [{ userId, displayName, email }]
    selected: [],
    candidates: [],
  };

  body.innerHTML = `
    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('invite_search_label'))}</span>
      <tf-searchbox id="ps-invite-search" placeholder="${escapeAttr(t('wizard_member_search'))}" debounce="250"></tf-searchbox>
      <div class="ps-selected-chips" data-invite-chips></div>
      <div class="ps-candidate-list" data-invite-candidates hidden></div>
    </div>
    <tf-select id="ps-invite-role" label="${escapeAttr(t('invite_role_label'))}" value="tester">
      ${ASSIGNABLE_ROLES.map((r) => `<option value="${r}" ${r === 'tester' ? 'selected' : ''}>${escapeHtml(roleLabel(r))}</option>`).join('')}
    </tf-select>
    <div class="ps-field-hint">${escapeHtml(t('invite_hint'))}</div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="primary" icon="plus" data-action="save">${escapeHtml(t('invite_submit'))}</tf-button>
    </div>
  `;

  const renderSelected = () => {
    const host = body.querySelector('[data-invite-chips]');
    if (!host) return;
    host.innerHTML = inv.selected.map((u, i) => `
      <tf-chip status="accent" removable data-invite-chip="${i}">${escapeHtml(u.displayName)}</tf-chip>
    `).join('');
    host.querySelectorAll('[data-invite-chip]').forEach((chip) => {
      chip.addEventListener('remove', () => {
        inv.selected.splice(Number(chip.dataset.inviteChip), 1);
        renderSelected();
        renderCandidates();
      });
    });
  };

  const renderCandidates = () => {
    const host = body.querySelector('[data-invite-candidates]');
    if (!host) return;
    const chosen = new Set(inv.selected.map((u) => u.userId));
    const rows = inv.candidates
      .filter((u) => !chosen.has(u.user_id ?? u.userId))
      .map((u) => {
        const userId = u.user_id ?? u.userId;
        return `
          <div class="ps-candidate-row" data-invite-candidate="${escapeAttr(userId)}" role="button" tabindex="0">
            <div class="ps-av-mini">${escapeHtml(initials(u.display_name ?? u.displayName))}</div>
            <div>
              <div class="ps-candidate-name">${escapeHtml(u.display_name ?? u.displayName ?? '')}</div>
              <div class="ps-candidate-mail">${escapeHtml(u.email || '')}</div>
            </div>
          </div>
        `;
      }).join('');
    host.hidden = !rows;
    host.innerHTML = rows;
  };

  body.querySelector('#ps-invite-search')?.addEventListener('search', async (e) => {
    const query = String(e.detail?.value ?? '').trim();
    if (!query) {
      inv.candidates = [];
      renderCandidates();
      return;
    }
    try {
      const resp = await ApiBinary.one('projectStudioMemberCandidatesRequest', {
        projectId: projectId(), query, limit: 12,
      });
      inv.candidates = Array.isArray(resp.users) ? resp.users : [];
    } catch {
      inv.candidates = [];
    }
    renderCandidates();
  });

  body.addEventListener('click', (e) => {
    const row = e.target.closest('[data-invite-candidate]');
    if (!row) return;
    const userId = row.dataset.inviteCandidate;
    const user = inv.candidates.find((u) => (u.user_id ?? u.userId) === userId);
    if (user) {
      inv.selected.push({
        userId,
        displayName: user.display_name ?? user.displayName ?? '',
        email: user.email || '',
      });
      renderSelected();
      renderCandidates();
    }
  });

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    const errEl = body.querySelector('[data-form-error]');
    if (!inv.selected.length) {
      if (errEl) { errEl.hidden = false; errEl.textContent = t('err_invite_empty'); }
      return;
    }
    const role = String(body.querySelector('#ps-invite-role')?.value || 'tester');
    try {
      const resp = await ApiBinary.one('projectStudioMembersAddRequest', {
        projectId: projectId(),
        members: inv.selected.map((u) => ({ userId: u.userId, role })),
      });
      toast(t('invite_ok', { count: Number(resp.added ?? inv.selected.length) }), 'success');
      cleanup();
      await renderMembers();
    } catch (err) {
      if (errEl) { errEl.hidden = false; errEl.textContent = `${t('invite_failed')}: ${err.message}`; }
    }
  });
}

// =============================================================================
// X04 — settings (basics, project agents, tags, danger zone)
// =============================================================================

async function renderSettings() {
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  try {
    const resp = await ApiBinary.one('projectStudioSettingsGetRequest', { projectId: projectId() });
    state.settings = resp.settings;
  } catch (err) {
    panel.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('settings_failed')}: ${err.message}`)}</div>`;
    return;
  }
  // The agent picker lists org agents; readable for non-admins too. A failure
  // degrades to showing only the current binding.
  try {
    const resp = await ApiBinary.one('agentsListRequest', {});
    const rows = JSON.parse(resp.agentsJson ?? resp.agents_json ?? '[]');
    state.agentOptions = Array.isArray(rows)
      ? rows.filter((a) => a.is_enabled).map((a) => ({ id: a.id, name: a.display_name || a.name }))
      : [];
  } catch {
    state.agentOptions = [];
  }
  if (state.tab !== 'settings') return;

  const settings = state.settings || {};
  const archived = state.project?.status === 'archived';
  const agents = Array.isArray(settings.agents) ? settings.agents : [];
  const chatBinding = agents.find((a) => a.function === 'chat') || { function: 'chat', agent_id: '', agent_name: '', model_label: '' };
  const chatAgentId = chatBinding.agent_id ?? chatBinding.agentId ?? '';
  const tags = Array.isArray(settings.tags) ? settings.tags : [];

  const agentOptions = [
    `<option value="" ${!chatAgentId ? 'selected' : ''}>${escapeHtml(t('agents_default_option'))}</option>`,
    ...state.agentOptions.map((a) => `<option value="${escapeAttr(a.id)}" ${a.id === chatAgentId ? 'selected' : ''}>${escapeHtml(a.name)}</option>`),
  ];
  // Keep an unknown current binding visible even when the agent list failed.
  if (chatAgentId && !state.agentOptions.some((a) => a.id === chatAgentId)) {
    agentOptions.push(`<option value="${escapeAttr(chatAgentId)}" selected>${escapeHtml(chatBinding.agent_name ?? chatBinding.agentName ?? chatAgentId)}</option>`);
  }

  panel.innerHTML = `
    <tf-section-card title="${escapeAttr(t('settings_basics_title'))}" icon="settings">
      <div class="ps-field" style="margin-bottom:12px;">
        <tf-input id="ps-set-name" label="${escapeAttr(t('wizard_name_label'))}" value="${escapeAttr(settings.name ?? state.project?.name ?? '')}"
          hint="${escapeAttr(t('settings_name_hint'))}"></tf-input>
      </div>
      <div class="ps-field" style="margin-bottom:12px;">
        <tf-textarea id="ps-set-desc" label="${escapeAttr(t('wizard_desc_label'))}" rows="3"
          hint="${escapeAttr(t('settings_desc_hint'))}"></tf-textarea>
      </div>
      <tf-button variant="primary" icon="check" id="ps-set-save">${escapeHtml(t('settings_save'))}</tf-button>
    </tf-section-card>

    <tf-section-card title="${escapeAttr(t('agents_title'))}" icon="brain">
      <span slot="subtitle">${escapeHtml(t('agents_sub'))}</span>
      <div class="ps-setting-row">
        <div class="ps-sr-main">
          <div class="ps-sr-label">${escapeHtml(t('agents_chat_label'))}</div>
          <div class="ps-sr-desc">${escapeHtml(t('agents_chat_desc'))}</div>
        </div>
        <div class="ps-fn-controls">
          <tf-select id="ps-agent-chat" value="${escapeAttr(chatAgentId)}">${agentOptions.join('')}</tf-select>
          <span class="ps-fn-model" id="ps-agent-chat-model">${escapeHtml((chatBinding.model_label ?? chatBinding.modelLabel) ? `${t('agents_model_prefix')}: ${chatBinding.model_label ?? chatBinding.modelLabel}` : '')}</span>
        </div>
      </div>
    </tf-section-card>

    <tf-section-card title="${escapeAttr(t('tags_title'))}" icon="filter">
      <span slot="subtitle">${escapeHtml(t('tags_sub'))}</span>
      <div class="ps-tag-add">
        <tf-input id="ps-tag-name" label="${escapeAttr(t('tags_new_label'))}" placeholder="${escapeAttr(t('tags_new_placeholder'))}"></tf-input>
        <tf-button variant="ghost" icon="plus" id="ps-tag-add">${escapeHtml(t('tags_add'))}</tf-button>
      </div>
      <div id="ps-tags-list">
        ${tags.length ? tags.map((tag) => `
          <div class="ps-tag-row" data-tag="${escapeAttr(tag.tag_id ?? tag.tagId)}">
            <tf-chip status="info">${escapeHtml(tag.name)}</tf-chip>
            <span class="ps-tag-count">${escapeHtml(t('tags_usage', { count: tag.usage_count ?? tag.usageCount ?? 0 }))}</span>
            <div class="ps-tag-actions">
              <tf-button variant="ghost" size="sm" icon="edit" data-tag-rename="${escapeAttr(tag.tag_id ?? tag.tagId)}" title="${escapeAttr(t('tags_rename'))}"></tf-button>
              <tf-button variant="ghost" size="sm" icon="trash" data-tag-delete="${escapeAttr(tag.tag_id ?? tag.tagId)}" title="${escapeAttr(t('action_delete'))}"></tf-button>
            </div>
          </div>
        `).join('') : `<div class="ps-field-hint">${escapeHtml(t('tags_empty'))}</div>`}
      </div>
    </tf-section-card>

    <div class="ps-danger-zone">
      <div class="ps-danger-title">${sprite('alert')}${escapeHtml(t('danger_title'))}</div>
      <div class="ps-danger-row">
        <div class="ps-sr-main">
          <div class="ps-sr-label">${escapeHtml(t(archived ? 'danger_unarchive_label' : 'danger_archive_label'))}</div>
          <div class="ps-sr-desc">${escapeHtml(t('danger_archive_desc'))}</div>
        </div>
        <tf-button variant="ghost" icon="clock" id="ps-danger-archive">${escapeHtml(t(archived ? 'action_unarchive' : 'action_archive'))}</tf-button>
      </div>
      <div class="ps-danger-row">
        <div class="ps-sr-main">
          <div class="ps-sr-label">${escapeHtml(t('danger_delete_label'))}</div>
          <div class="ps-sr-desc">${escapeHtml(t('danger_delete_desc'))}</div>
        </div>
        <tf-button variant="danger-solid" icon="trash" id="ps-danger-delete" ${isOwner() ? '' : `disabled title="${escapeAttr(t('danger_owner_only'))}"`}>${escapeHtml(t('action_delete'))}</tf-button>
      </div>
    </div>
  `;

  const descField = panel.querySelector('#ps-set-desc');
  if (descField) descField.value = settings.description ?? state.project?.description ?? '';

  byId('ps-set-save')?.addEventListener('click', async () => {
    const name = String(byId('ps-set-name')?.value ?? '').trim();
    const description = String(byId('ps-set-desc')?.value ?? '').trim();
    if (name.length < 3) {
      toast(t('err_name_short'), 'error');
      return;
    }
    try {
      await ApiBinary.one('projectStudioSettingsSaveRequest', { projectId: projectId(), name, description });
      toast(t('settings_saved'), 'success');
      await refreshProjectHeader();
    } catch (err) {
      toast(`${t('settings_save_failed')}: ${err.message}`, 'error');
    }
  });

  byId('ps-agent-chat')?.addEventListener('change', async (e) => {
    const agentId = e.detail?.value ?? e.target.value ?? '';
    try {
      await ApiBinary.one('projectStudioSettingsSaveRequest', {
        projectId: projectId(),
        agentsJson: JSON.stringify([{ function: 'chat', agent_id: agentId }]),
      });
      toast(t('agents_saved'), 'success');
      // Re-fetch so the resolved model label next to the select stays honest.
      const resp = await ApiBinary.one('projectStudioSettingsGetRequest', { projectId: projectId() });
      const binding = (resp.settings?.agents ?? []).find((a) => a.function === 'chat');
      const modelEl = byId('ps-agent-chat-model');
      const modelLabel = binding ? (binding.model_label ?? binding.modelLabel ?? '') : '';
      if (modelEl) modelEl.textContent = modelLabel ? `${t('agents_model_prefix')}: ${modelLabel}` : '';
    } catch (err) {
      toast(`${t('agents_save_failed')}: ${err.message}`, 'error');
    }
  });

  byId('ps-tag-add')?.addEventListener('click', async () => {
    const name = String(byId('ps-tag-name')?.value ?? '').trim();
    if (!name) return;
    try {
      await ApiBinary.one('projectStudioTagSaveRequest', { projectId: projectId(), name });
      toast(t('tags_added'), 'success');
      await renderSettings();
    } catch (err) {
      toast(`${t('tags_save_failed')}: ${err.message}`, 'error');
    }
  });

  byId('ps-tags-list')?.addEventListener('click', async (e) => {
    const renameBtn = e.target.closest('[data-tag-rename]');
    if (renameBtn) {
      const tagId = renameBtn.dataset.tagRename;
      const tag = (state.settings?.tags ?? []).find((x) => (x.tag_id ?? x.tagId) === tagId);
      const name = await openPromptWindow({
        title: t('tags_rename'),
        label: t('tags_new_label'),
        value: tag?.name || '',
      });
      if (name == null || !name) return;
      try {
        await ApiBinary.one('projectStudioTagSaveRequest', { projectId: projectId(), tagId, name });
        toast(t('tags_saved'), 'success');
        await renderSettings();
      } catch (err) {
        toast(`${t('tags_save_failed')}: ${err.message}`, 'error');
      }
      return;
    }
    const deleteBtn = e.target.closest('[data-tag-delete]');
    if (deleteBtn) {
      const tagId = deleteBtn.dataset.tagDelete;
      const tag = (state.settings?.tags ?? []).find((x) => (x.tag_id ?? x.tagId) === tagId);
      const ok = await TfWindow.confirm({
        title: t('tags_delete_title'),
        message: t('tags_delete_message', { name: escapeHtml(tag?.name || '') }),
        confirmLabel: t('action_delete'),
        cancelLabel: t('action_cancel'),
        danger: true,
      });
      if (!ok) return;
      try {
        await ApiBinary.one('projectStudioTagDeleteRequest', { projectId: projectId(), tagId });
        toast(t('tags_deleted'), 'success');
        await renderSettings();
      } catch (err) {
        toast(`${t('tags_delete_failed')}: ${err.message}`, 'error');
      }
    }
  });

  byId('ps-danger-archive')?.addEventListener('click', () => {
    setProjectArchived(projectId(), !archived);
  });
  byId('ps-danger-delete')?.addEventListener('click', () => {
    if (isOwner()) confirmDeleteProject(state.project);
  });
}

// =============================================================================
// F2 — shared helpers (wire field access, chips, tags, members, attachments)
// =============================================================================

const F2_PAGE_SIZE = 25;
const GEN_POLL_MS = 3000;
const ATTACHMENT_MAX_PREVIEW = 8 * 1024 * 1024;

const CASE_STATUS_CHIP = { draft: 'info', review: 'warn', approved: 'ok', deprecated: 'err' };
const PRIORITY_CHIP = { low: 'info', medium: 'accent', high: 'warn', critical: 'err' };
const RUN_STATUS_CHIP = { running: 'accent', completed: 'ok', cancelled: 'warn' };
const ITEM_STATUS_CHIP = { pending: 'info', in_progress: 'accent', passed: 'ok', failed: 'err', blocked: 'warn', skipped: 'info' };
const GEN_STATUS_CHIP = { running: 'accent', review: 'warn', accepted: 'ok', rejected: 'err', failed: 'err', cancelled: 'info' };
const TASK_STATUS_CHIP = { todo: 'info', in_progress: 'accent', review: 'warn', done: 'ok' };
const CASE_KINDS = ['manual', 'ui', 'api', 'unit', 'perf', 'security'];
const PRIORITIES = ['low', 'medium', 'high', 'critical'];
const TASK_STATUSES = ['todo', 'in_progress', 'review', 'done'];
const NOTIF_KIND_ICON = {
  run_item_assigned: 'play',
  run_closed: 'check',
  generation_finished: 'sparkle',
  task_assigned: 'edit',
};

// Wire structs decode with snake_case field names; some decode layers expose
// camelCase aliases instead — read both so the UI never depends on which one
// the glue produced.
function fv(obj, key) {
  if (!obj || typeof obj !== 'object') return undefined;
  if (obj[key] !== undefined) return obj[key];
  const camel = key.replace(/_([a-z0-9])/g, (_, c) => c.toUpperCase());
  return obj[camel];
}

function chipCell(status, label) {
  return { status: status || 'info', label };
}

function caseStatusChipHtml(status) {
  return `<tf-chip status="${CASE_STATUS_CHIP[status] || 'info'}" dot>${escapeHtml(t(`case_status_${status}`))}</tf-chip>`;
}

function f2() {
  if (!state.f2) state.f2 = freshF2State();
  return state.f2;
}

// Project tags come from the settings payload; a viewer without settings
// access simply gets an empty tag catalog (pickers degrade gracefully).
async function loadProjectTags(force = false) {
  const s = f2();
  if (s.tagsLoaded && !force) return s.tags;
  try {
    const resp = await ApiBinary.one('projectStudioSettingsGetRequest', { projectId: projectId() });
    const tags = fv(resp.settings ?? {}, 'tags');
    s.tags = Array.isArray(tags) ? tags : [];
  } catch {
    s.tags = [];
  }
  s.tagsLoaded = true;
  return s.tags;
}

function tagNameById(tagId) {
  const tag = f2().tags.find((x) => fv(x, 'tag_id') === tagId);
  return tag ? tag.name : '';
}

async function ensureF2Members() {
  const s = f2();
  if (s.membersCache) return s.membersCache;
  try {
    const resp = await ApiBinary.one('projectStudioMembersListRequest', { projectId: projectId() });
    s.membersCache = Array.isArray(resp.members) ? resp.members : [];
  } catch {
    s.membersCache = [];
  }
  return s.membersCache;
}

// Members allowed to execute tests (tester and above).
function testerMembers() {
  return (f2().membersCache || []).filter((m) => (ROLE_RANK[m.role] ?? 0) >= ROLE_RANK.tester);
}

function memberName(userId) {
  const m = (f2().membersCache || []).find((x) => fv(x, 'user_id') === userId);
  return m ? (fv(m, 'display_name') || '') : '';
}

// Uploads one attachment through the shared chunked-upload channel and returns
// the AttachmentWire shape stored in attachments_json. The final chunk response
// carries the content-hash file ref (optionally prefixed with "sha256:").
async function uploadAttachmentFile(file) {
  const uploadId = crypto.randomUUID();
  const buffer = new Uint8Array(await file.arrayBuffer());
  const totalChunks = Math.max(1, Math.ceil(buffer.length / UPLOAD_CHUNK_BYTES));
  let fileRef = null;
  for (let seq = 0; seq < totalChunks; seq += 1) {
    const chunk = buffer.subarray(seq * UPLOAD_CHUNK_BYTES, Math.min((seq + 1) * UPLOAD_CHUNK_BYTES, buffer.length));
    const resp = await ApiBinary.one('projectStudioSourceUploadChunkRequest', {
      projectId: projectId(),
      uploadId,
      filename: file.name,
      mime: file.type || 'application/octet-stream',
      seq,
      totalChunks,
      bytes: chunk,
    });
    fileRef = fv(resp, 'file_ref') ?? fileRef;
  }
  if (!fileRef) throw new Error(t('upload_no_ref'));
  return {
    sha256: String(fileRef).replace(/^sha256:/, ''),
    name: file.name,
    size_bytes: file.size,
    mime: file.type || 'application/octet-stream',
  };
}

async function fetchAttachmentBlob(att) {
  const resp = await ApiBinary.one('projectStudioAttachmentGetRequest', {
    projectId: projectId(),
    sha256: fv(att, 'sha256'),
    maxBytes: ATTACHMENT_MAX_PREVIEW,
  });
  const bytes = resp.bytes instanceof Uint8Array ? resp.bytes : new Uint8Array(resp.bytes || []);
  const mime = resp.mime || fv(att, 'mime') || 'application/octet-stream';
  return { blob: new Blob([bytes], { type: mime }), mime, truncated: !!resp.truncated };
}

// Preview window for images / plain text; other mime types download directly.
async function openAttachmentPreview(att) {
  let fetched = null;
  try {
    fetched = await fetchAttachmentBlob(att);
  } catch (err) {
    toast(`${t('attachment_failed')}: ${err.message}`, 'error');
    return;
  }
  const url = URL.createObjectURL(fetched.blob);
  const name = fv(att, 'name') || 'attachment';
  if (fetched.mime.startsWith('image/')) {
    const { body, foot, cleanup } = openWindow({ title: name, subtitle: fetched.mime, icon: 'image', width: 820 });
    body.innerHTML = `<div class="ps-att-preview"><img alt="${escapeAttr(name)}"></div>`;
    body.querySelector('img').src = url;
    foot.innerHTML = `
      <div class="ps-footer-left"></div>
      <div class="ps-footer-right">
        <tf-button variant="ghost" icon="download" data-action="download">${escapeHtml(t('attachment_download'))}</tf-button>
        <tf-button variant="ghost" data-action="close-att">${escapeHtml(t('action_close'))}</tf-button>
      </div>
    `;
    foot.addEventListener('click', (e) => {
      const btn = e.target.closest('[data-action]');
      if (!btn) return;
      if (btn.dataset.action === 'download') downloadUrl(url, name);
      else { cleanup(); URL.revokeObjectURL(url); }
    });
    return;
  }
  downloadUrl(url, name);
  setTimeout(() => URL.revokeObjectURL(url), 30000);
}

function downloadUrl(url, name) {
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
}

function downloadTextFile(name, text, mime = 'text/csv') {
  const url = URL.createObjectURL(new Blob([text], { type: `${mime};charset=utf-8` }));
  downloadUrl(url, name);
  setTimeout(() => URL.revokeObjectURL(url), 30000);
}

// Minimal CSV writer with RFC-4180 quoting; used by run results / reports
// exports (rows_json is exported client-side by design in F2).
function toCsv(rows, columns) {
  const cols = columns || [...rows.reduce((set, row) => {
    Object.keys(row || {}).forEach((k) => set.add(k));
    return set;
  }, new Set())];
  const quote = (v) => {
    let s = v == null ? '' : String(v);
    // Spreadsheets execute cells starting with =, +, -, @ or tab as formulas —
    // prefix an apostrophe so exported user content stays inert.
    if (/^[=+\-@\t]/.test(s)) s = `'${s}`;
    return /[",\n;]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  const lines = [cols.map(quote).join(';')];
  for (const row of rows) lines.push(cols.map((c) => quote(row?.[c])).join(';'));
  return lines.join('\n');
}

function attachmentRowHtml(att, index, { removable = false } = {}) {
  return `
    <div class="ps-added-file" data-att-index="${index}">
      <span class="ps-af-ico">${sprite((fv(att, 'mime') || '').startsWith('image/') ? 'image' : 'file-text')}</span>
      <div class="ps-af-main">
        <div class="ps-af-name">${escapeHtml(fv(att, 'name') || '')}</div>
        <div class="ps-af-size">${escapeHtml(formatBytes(Number(fv(att, 'size_bytes') ?? 0)))}</div>
      </div>
      <tf-button variant="ghost" size="sm" icon="eye" data-att-preview="${index}" title="${escapeAttr(t('kb_preview'))}"></tf-button>
      ${removable ? `<tf-button variant="ghost" size="sm" icon="trash" data-att-remove="${index}" title="${escapeAttr(t('action_delete'))}"></tf-button>` : ''}
    </div>
  `;
}

// =============================================================================
// F2 — tests tab shell (sub-nav: cases / suites / runs / generations / reports)
// =============================================================================

const TESTS_SEGMENTS = ['cases', 'suites', 'runs', 'generations', 'reports'];

// Drill-in views highlight their parent segment in the sub-nav.
function testsSegValue() {
  const view = f2().view;
  if (view === 'case-editor') return 'cases';
  if (view === 'suite-editor') return 'suites';
  if (view === 'run-detail' || view === 'exec') return 'runs';
  if (view === 'gen-detail') return 'generations';
  return TESTS_SEGMENTS.includes(view) ? view : 'cases';
}

async function renderTests() {
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  panel.innerHTML = `
    <div class="ps-subnav">
      <tf-segmented id="ps-tests-seg" value="${escapeAttr(testsSegValue())}">
        ${TESTS_SEGMENTS.map((seg) => `<option value="${seg}">${escapeHtml(t(`tests_seg_${seg}`))}</option>`).join('')}
      </tf-segmented>
      <span class="ps-subnav-hint">${escapeHtml(t('tests_subnav_hint'))}</span>
    </div>
    <div id="ps-tests-host"></div>
  `;
  byId('ps-tests-seg')?.addEventListener('change', (e) => {
    const view = e.detail?.value;
    if (!view || view === testsSegValue()) return;
    stopTestsLive();
    f2().view = view;
    renderTestsView();
  });
  await renderTestsView();
}

function syncTestsSeg() {
  byId('ps-tests-seg')?.setAttribute('value', testsSegValue());
}

async function renderTestsView() {
  const host = byId('ps-tests-host');
  if (!host) return;
  syncTestsSeg();
  host.innerHTML = `<div class="ps-loading">${escapeHtml(t('loading'))}</div>`;
  switch (f2().view) {
    case 'cases': await renderCasesView(); break;
    case 'case-editor': await renderCaseEditor(); break;
    case 'suites': await renderSuitesView(); break;
    case 'suite-editor': await renderSuiteEditor(); break;
    case 'runs': await renderRunsView(); break;
    case 'run-detail': await renderRunDetailView(); break;
    case 'exec': await renderExecView(); break;
    case 'generations': await renderGenerationsView(); break;
    case 'gen-detail': await renderGenDetailView(); break;
    case 'reports': await renderReportsView(); break;
    default: f2().view = 'cases'; await renderCasesView(); break;
  }
}

// Tears down every live artifact of the tests tab: the generation event
// stream, the generation poll and the execution duration ticker.
function stopTestsLive() {
  const s = state.f2;
  if (!s) return;
  if (s.genUnsub) { try { s.genUnsub(); } catch { /* stream already gone */ } s.genUnsub = null; }
  if (s.genPollTimer) { clearInterval(s.genPollTimer); s.genPollTimer = null; }
  s.genWidget = null;
  s.genSteps = [];
  if (s.exec?.timerId) { clearInterval(s.exec.timerId); s.exec.timerId = null; }
}

// =============================================================================
// T01 — test cases list (filters, bulk actions, pagination, CSV import)
// =============================================================================

async function loadCasesPage() {
  const s = f2();
  const flt = s.cases.filters;
  const resp = await ApiBinary.one('projectStudioCasesListRequest', {
    projectId: projectId(),
    kind: flt.kind,
    status: flt.status,
    priority: flt.priority,
    tagId: flt.tagId,
    origin: flt.origin,
    search: flt.search,
    offset: (s.cases.page - 1) * F2_PAGE_SIZE,
    limit: F2_PAGE_SIZE,
  });
  s.cases.rows = Array.isArray(resp.cases) ? resp.cases : [];
  s.cases.total = Number(resp.total ?? s.cases.rows.length);
}

async function renderCasesView() {
  const host = byId('ps-tests-host');
  if (!host) return;
  const s = f2();
  await loadProjectTags();
  try {
    await loadCasesPage();
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('cases_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || s.view !== 'cases') return;
  const flt = s.cases.filters;

  const selectOpt = (value, current, label) =>
    `<option value="${escapeAttr(value)}" ${value === current ? 'selected' : ''}>${escapeHtml(label)}</option>`;

  host.innerHTML = `
    <div class="ps-tests-toolbar">
      <tf-searchbox id="ps-cases-search" placeholder="${escapeAttr(t('cases_search_placeholder'))}" debounce="300" value="${escapeAttr(flt.search)}"></tf-searchbox>
      <tf-select id="ps-cases-f-kind" value="${escapeAttr(flt.kind)}">
        ${selectOpt('', flt.kind, t('cases_filter_kind_all'))}
        ${CASE_KINDS.map((k) => selectOpt(k, flt.kind, t(`case_kind_${k}`))).join('')}
      </tf-select>
      <tf-select id="ps-cases-f-status" value="${escapeAttr(flt.status)}">
        ${selectOpt('', flt.status, t('cases_filter_status_all'))}
        ${['draft', 'review', 'approved', 'deprecated'].map((x) => selectOpt(x, flt.status, t(`case_status_${x}`))).join('')}
      </tf-select>
      <tf-select id="ps-cases-f-priority" value="${escapeAttr(flt.priority)}">
        ${selectOpt('', flt.priority, t('cases_filter_priority_all'))}
        ${PRIORITIES.map((x) => selectOpt(x, flt.priority, t(`prio_${x}`))).join('')}
      </tf-select>
      <tf-select id="ps-cases-f-tag" value="${escapeAttr(flt.tagId)}">
        ${selectOpt('', flt.tagId, t('cases_filter_tag_all'))}
        ${s.tags.map((tag) => selectOpt(fv(tag, 'tag_id'), flt.tagId, tag.name)).join('')}
      </tf-select>
      <tf-select id="ps-cases-f-origin" value="${escapeAttr(flt.origin)}">
        ${selectOpt('', flt.origin, t('cases_filter_origin_all'))}
        ${selectOpt('user', flt.origin, t('origin_user'))}
        ${selectOpt('agent', flt.origin, t('origin_agent'))}
      </tf-select>
      <span class="ps-toolbar-spacer"></span>
      ${canEdit() ? `
        <tf-button variant="ghost" icon="download" id="ps-cases-import">${escapeHtml(t('cases_import_csv'))}</tf-button>
        <tf-button variant="ghost" icon="sparkle" id="ps-cases-generate">${escapeHtml(t('cases_generate'))}</tf-button>
        <tf-button variant="primary" icon="plus" id="ps-cases-new">${escapeHtml(t('cases_new'))}</tf-button>
      ` : ''}
    </div>
    <div class="ps-bulk-bar" id="ps-cases-bulk" hidden>
      <span class="ps-bulk-count" id="ps-cases-bulk-count"></span>
      ${canEdit() ? `<tf-button variant="ghost" size="sm" icon="send" data-bulk="review">${escapeHtml(t('bulk_to_review'))}</tf-button>` : ''}
      ${canManage() ? `<tf-button variant="ghost" size="sm" icon="check" data-bulk="approved">${escapeHtml(t('bulk_approve'))}</tf-button>` : ''}
      ${canManage() ? `<tf-button variant="ghost" size="sm" icon="ban" data-bulk="deprecated">${escapeHtml(t('bulk_deprecate'))}</tf-button>` : ''}
      ${canEdit() ? `<tf-button variant="ghost" size="sm" icon="trash" data-bulk="delete">${escapeHtml(t('bulk_delete'))}</tf-button>` : ''}
      <tf-button variant="ghost" size="sm" icon="x" data-bulk="clear">${escapeHtml(t('bulk_clear'))}</tf-button>
    </div>
    <div id="ps-cases-table-host"></div>
  `;

  const reload = () => { s.cases.page = 1; s.cases.selected = new Set(); renderCasesView(); };
  byId('ps-cases-search')?.addEventListener('search', (e) => { flt.search = String(e.detail?.value ?? ''); reload(); });
  const bindFilter = (id, key) => {
    byId(id)?.addEventListener('change', (e) => { flt[key] = e.detail?.value ?? e.target.value ?? ''; reload(); });
  };
  bindFilter('ps-cases-f-kind', 'kind');
  bindFilter('ps-cases-f-status', 'status');
  bindFilter('ps-cases-f-priority', 'priority');
  bindFilter('ps-cases-f-tag', 'tagId');
  bindFilter('ps-cases-f-origin', 'origin');
  byId('ps-cases-import')?.addEventListener('click', () => openCsvImportWindow());
  byId('ps-cases-generate')?.addEventListener('click', () => openGenerationWindow());
  byId('ps-cases-new')?.addEventListener('click', () => openCaseEditor(null));
  byId('ps-cases-bulk')?.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-bulk]');
    if (btn) handleCasesBulk(btn.dataset.bulk);
  });

  renderCasesTable();
}

function renderCasesTable() {
  const s = f2();
  const tableHost = byId('ps-cases-table-host');
  if (!tableHost) return;
  if (!s.cases.rows.length) {
    tableHost.innerHTML = `<tf-empty-state icon="list" title="${escapeAttr(t('cases_empty'))}"></tf-empty-state>`;
    updateCasesBulkBar();
    return;
  }
  tableHost.innerHTML = `
    <tf-table id="ps-cases-table" selectable="multi" sortable page-size="${F2_PAGE_SIZE}" total="${s.cases.total}" page="${s.cases.page}">
      <tf-column key="sel" label=""></tf-column>
      <tf-column key="title" label="${escapeAttr(t('cases_col_title'))}" sortable></tf-column>
      <tf-column key="kind" label="${escapeAttr(t('cases_col_kind'))}" renderer="chip"></tf-column>
      <tf-column key="priority" label="${escapeAttr(t('cases_col_priority'))}" renderer="chip"></tf-column>
      <tf-column key="tags" label="${escapeAttr(t('cases_col_tags'))}"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('cases_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="origin" label="${escapeAttr(t('cases_col_origin'))}" renderer="html"></tf-column>
      <tf-column key="lastResult" label="${escapeAttr(t('cases_col_last_result'))}" renderer="chip"></tf-column>
      <tf-column key="updated" label="${escapeAttr(t('cases_col_updated'))}" sortable></tf-column>
    </tf-table>
  `;
  const table = byId('ps-cases-table');
  const assignRows = () => {
    table.rows = s.cases.rows.map((c) => {
      const caseId = fv(c, 'case_id');
      const lastResult = fv(c, 'last_result');
      return {
        _id: caseId,
        _row: c,
        sel: s.cases.selected.has(caseId) ? '✓' : '',
        title: c.title,
        kind: chipCell('info', t(`case_kind_${c.kind}`)),
        priority: chipCell(PRIORITY_CHIP[c.priority], t(`prio_${c.priority}`)),
        tags: (fv(c, 'tag_ids') || []).map(tagNameById).filter(Boolean).join(', ') || '—',
        status: chipCell(CASE_STATUS_CHIP[c.status], t(`case_status_${c.status}`)),
        origin: c.origin === 'agent'
          ? `<span class="ps-origin-agent" title="${escapeAttr(t('origin_agent_tooltip'))}">${sprite('sparkle')}</span>`
          : `<span class="ps-origin-user">${sprite('user')}</span>`,
        lastResult: lastResult
          ? chipCell(ITEM_STATUS_CHIP[lastResult], t(`item_status_${lastResult}`))
          : chipCell('info', '—'),
        updated: formatTimestamp(fv(c, 'updated_at')),
      };
    });
  };
  assignRows();
  table.rowActions = (row) => {
    const wrap = document.createElement('div');
    wrap.className = 'ps-file-actions';
    const mk = (icon, title, handler) => {
      const btn = document.createElement('tf-button');
      btn.setAttribute('variant', 'ghost');
      btn.setAttribute('size', 'sm');
      btn.setAttribute('icon', icon);
      btn.setAttribute('title', title);
      btn.addEventListener('click', (e) => { e.stopPropagation(); handler(); });
      wrap.appendChild(btn);
    };
    mk('edit', t(canEdit() ? 'action_edit' : 'cases_open'), () => openCaseEditor(row._id));
    if (canEdit()) {
      mk('copy', t('cases_duplicate'), () => duplicateCase(row._id));
      mk('trash', t('action_delete'), () => deleteCaseFromList(row._row));
    }
    return wrap;
  };
  table.addEventListener('row-click', (e) => {
    const caseId = e.detail?.row?._id;
    if (!caseId) return;
    if (s.cases.selected.has(caseId)) s.cases.selected.delete(caseId);
    else s.cases.selected.add(caseId);
    assignRows();
    updateCasesBulkBar();
  });
  table.addEventListener('select-all', (e) => {
    if (e.detail?.selected) s.cases.rows.forEach((c) => s.cases.selected.add(fv(c, 'case_id')));
    else s.cases.selected = new Set();
    assignRows();
    updateCasesBulkBar();
  });
  table.addEventListener('page-change', async (e) => {
    s.cases.page = Number(e.detail?.page ?? 1);
    try {
      await loadCasesPage();
    } catch (err) {
      toast(`${t('cases_failed')}: ${err.message}`, 'error');
      return;
    }
    table.setAttribute('page', String(s.cases.page));
    table.setAttribute('total', String(s.cases.total));
    assignRows();
  });
  updateCasesBulkBar();
}

function updateCasesBulkBar() {
  const s = f2();
  const bar = byId('ps-cases-bulk');
  if (!bar) return;
  const count = s.cases.selected.size;
  bar.hidden = count === 0;
  const label = byId('ps-cases-bulk-count');
  if (label) label.textContent = t('bulk_selected', { count });
}

async function handleCasesBulk(action) {
  const s = f2();
  const ids = [...s.cases.selected];
  if (!ids.length) return;
  if (action === 'clear') {
    s.cases.selected = new Set();
    renderCasesTable();
    return;
  }
  if (action === 'delete') {
    const ok = await TfWindow.confirm({
      title: t('bulk_delete_title'),
      message: t('bulk_delete_message', { count: ids.length }),
      confirmLabel: t('action_delete'),
      cancelLabel: t('action_cancel'),
      danger: true,
    });
    if (!ok) return;
    let deleted = 0;
    let lastError = null;
    for (const caseId of ids) {
      try {
        await ApiBinary.one('projectStudioCaseDeleteRequest', { projectId: projectId(), caseId });
        deleted += 1;
      } catch (err) {
        lastError = err;
      }
    }
    if (deleted) toast(t('bulk_delete_ok', { count: deleted }), 'success');
    if (lastError) toast(`${t('bulk_delete_partial')}: ${lastError.message}`, 'error');
    s.cases.selected = new Set();
    await renderCasesView();
    return;
  }
  // Status transitions. Every downgrade (→ deprecated) requires a reason.
  let reason = '';
  if (action === 'deprecated') {
    reason = await openPromptWindow({ title: t('bulk_deprecate'), label: t('status_reason_label'), icon: 'alert' });
    if (reason == null || !reason) return;
  }
  try {
    const resp = await ApiBinary.one('projectStudioCasesBulkStatusRequest', {
      projectId: projectId(), caseIds: ids, status: action, reason,
    });
    toast(t('bulk_status_ok', { count: Number(resp.updated ?? 0) }), 'success');
    s.cases.selected = new Set();
    await renderCasesView();
  } catch (err) {
    toast(`${t('bulk_status_failed')}: ${err.message}`, 'error');
  }
}

async function duplicateCase(caseId) {
  try {
    const resp = await ApiBinary.one('projectStudioCaseDuplicateRequest', { projectId: projectId(), caseId });
    toast(t('cases_duplicated'), 'success');
    const newId = fv(resp, 'case_id');
    if (newId) openCaseEditor(newId);
  } catch (err) {
    toast(`${t('cases_duplicate_failed')}: ${err.message}`, 'error');
  }
}

function deleteCaseFromList(caseRow) {
  openDeleteWindow({
    title: t('case_delete_title'),
    targetName: caseRow.title,
    targetSub: t(`case_status_${caseRow.status}`),
    targetIcon: 'list',
    warning: t('case_delete_warning'),
    confirmLabel: t('delete_forever'),
    onConfirm: async () => {
      await ApiBinary.one('projectStudioCaseDeleteRequest', { projectId: projectId(), caseId: fv(caseRow, 'case_id') });
      toast(t('case_delete_ok'), 'success');
      await renderCasesView();
    },
  });
}

// ---- CSV import (dry run first, per-line errors) ----------------------------

function openCsvImportWindow() {
  const { body, foot, cleanup } = openWindow({
    title: t('csv_title'),
    subtitle: t('csv_subtitle'),
    icon: 'download',
    width: 640,
  });
  const st = { text: '', checked: false, hasErrors: false, busy: false };

  body.innerHTML = `
    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('csv_file_label'))}</span>
      <tf-file-input id="ps-csv-file" accept=".csv,text/csv" label="${escapeAttr(t('csv_dropzone'))}"></tf-file-input>
      <div class="ps-field-hint">${escapeHtml(t('csv_format_hint'))}</div>
    </div>
    <div id="ps-csv-result"></div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="ghost" icon="eye" data-action="dry" disabled>${escapeHtml(t('csv_check'))}</tf-button>
      <tf-button variant="primary" icon="check" data-action="import" disabled>${escapeHtml(t('csv_import'))}</tf-button>
    </div>
  `;

  const dryBtn = foot.querySelector('[data-action="dry"]');
  const importBtn = foot.querySelector('[data-action="import"]');
  const resultHost = body.querySelector('#ps-csv-result');
  const errEl = body.querySelector('[data-form-error]');
  const showError = (msg) => { errEl.hidden = !msg; errEl.textContent = msg || ''; };

  const renderResult = (created, errors, dry) => {
    const errRows = (errors || []).map((e) => `
      <div class="ps-csv-err-row">
        <tf-chip status="err">${escapeHtml(t('csv_line', { line: fv(e, 'line') }))}</tf-chip>
        <span>${escapeHtml(e.message || '')}</span>
      </div>
    `).join('');
    resultHost.innerHTML = `
      <div class="ps-banner-info">${sprite(errors?.length ? 'alert' : 'check')}<span>
        ${escapeHtml(dry ? t('csv_dry_result', { count: created, errors: errors?.length ?? 0 }) : t('csv_import_result', { count: created }))}
      </span></div>
      ${errRows ? `<div class="ps-csv-errors">${errRows}</div>` : ''}
    `;
  };

  body.querySelector('#ps-csv-file')?.addEventListener('change', async (e) => {
    const files = e.detail?.files;
    const file = files && files[0];
    if (!file) return;
    st.text = await file.text();
    st.checked = false;
    dryBtn.removeAttribute('disabled');
    importBtn.setAttribute('disabled', '');
    resultHost.innerHTML = '';
    showError(null);
  });

  const runImport = async (dryRun) => {
    if (st.busy || !st.text) return;
    st.busy = true;
    showError(null);
    try {
      const resp = await ApiBinary.one('projectStudioCasesImportCsvRequest', {
        projectId: projectId(), csvText: st.text, dryRun,
      });
      const created = Number(resp.created ?? 0);
      const errors = Array.isArray(resp.errors) ? resp.errors : [];
      renderResult(created, errors, dryRun);
      if (dryRun) {
        st.checked = true;
        st.hasErrors = errors.length > 0;
        if (!st.hasErrors && created > 0) importBtn.removeAttribute('disabled');
        else importBtn.setAttribute('disabled', '');
      } else {
        toast(t('csv_import_ok', { count: created }), 'success');
        cleanup();
        await renderCasesView();
      }
    } catch (err) {
      showError(`${t('csv_failed')}: ${err.message}`);
    } finally {
      st.busy = false;
    }
  };

  foot.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || btn.hasAttribute('disabled')) return;
    if (btn.dataset.action === 'cancel') cleanup();
    else if (btn.dataset.action === 'dry') runImport(true);
    else if (btn.dataset.action === 'import') runImport(false);
  });
}

// =============================================================================
// T02 — manual case editor (steps, versions, attachments, optimistic locking)
// =============================================================================

function openCaseEditor(caseId) {
  f2().editor = {
    caseId: caseId || null,
    loaded: !caseId,
    info: null,
    title: '',
    priority: 'medium',
    preconditions: '',
    testData: '',
    steps: caseId ? [] : [{ action: '', expected: '' }],
    tagNames: [],
    linkedSourceIds: new Set(),
    attachments: [],
    versions: [],
    expectedVersion: null,
    changeNote: '',
    uploading: false,
  };
  f2().view = 'case-editor';
  renderTestsView();
}

function parseCaseContent(contentJson) {
  let content = {};
  try { content = JSON.parse(contentJson || '{}') || {}; } catch { content = {}; }
  const steps = Array.isArray(content.steps)
    ? content.steps.map((s) => ({ action: String(s.action ?? ''), expected: String(s.expected ?? '') }))
    : [];
  return {
    preconditions: String(content.preconditions ?? ''),
    testData: String(content.test_data ?? content.testData ?? ''),
    steps: steps.length ? steps : [{ action: '', expected: '' }],
  };
}

async function loadCaseIntoEditor(ed) {
  const resp = await ApiBinary.one('projectStudioCaseGetRequest', {
    projectId: projectId(), caseId: ed.caseId, includeVersions: true,
  });
  const detail = resp.detail || {};
  const info = detail.info || {};
  const content = parseCaseContent(fv(detail, 'content_json'));
  ed.info = info;
  ed.title = info.title || '';
  ed.priority = info.priority || 'medium';
  ed.preconditions = content.preconditions;
  ed.testData = content.testData;
  ed.steps = content.steps;
  ed.tagNames = (fv(info, 'tag_ids') || []).map(tagNameById).filter(Boolean);
  ed.linkedSourceIds = new Set(fv(info, 'linked_source_ids') || []);
  ed.attachments = Array.isArray(detail.attachments) ? detail.attachments.slice() : [];
  ed.versions = Array.isArray(detail.versions) ? detail.versions.slice() : [];
  ed.expectedVersion = Number(fv(info, 'current_version') ?? 1);
  ed.loaded = true;
}

async function renderCaseEditor() {
  const host = byId('ps-tests-host');
  const ed = f2().editor;
  if (!host || !ed) { f2().view = 'cases'; return renderCasesView(); }
  await loadProjectTags();
  if (!state.sources.length) {
    try { await loadSources(); } catch { /* linked-source picker stays empty */ }
  }
  if (!ed.loaded) {
    try {
      await loadCaseIntoEditor(ed);
    } catch (err) {
      host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('case_load_failed')}: ${err.message}`)}</div>`;
      return;
    }
  }
  if (state.tab !== 'tests' || f2().view !== 'case-editor') return;

  const info = ed.info;
  const status = info?.status || 'draft';
  const editable = canEdit() && (!info || status === 'draft' || status === 'review');
  const statusActions = [];
  if (info) {
    if (status === 'draft' && canEdit()) {
      statusActions.push(`<tf-button variant="ghost" icon="send" data-status="review">${escapeHtml(t('case_send_review'))}</tf-button>`);
    }
    if (status === 'review' && canManage()) {
      statusActions.push(`<tf-button variant="ghost" icon="check" data-status="approved">${escapeHtml(t('case_approve'))}</tf-button>`);
    }
    if (status === 'review' && canManage()) {
      statusActions.push(`<tf-button variant="ghost" icon="chevron-left" data-status="draft" data-needs-reason>${escapeHtml(t('case_back_to_draft'))}</tf-button>`);
    }
    if (status === 'approved' && canManage()) {
      statusActions.push(`<tf-button variant="ghost" icon="ban" data-status="deprecated" data-needs-reason>${escapeHtml(t('case_deprecate'))}</tf-button>`);
    }
  }

  host.innerHTML = `
    <div class="ps-editor-head">
      <tf-button variant="ghost" icon="chevron-left" id="ps-case-back">${escapeHtml(t('back_to_cases'))}</tf-button>
      <div class="ps-editor-title">
        <tf-input id="ps-case-title" label="${escapeAttr(t('case_title_label'))}" value="${escapeAttr(ed.title)}" ${editable ? '' : 'readonly'}></tf-input>
      </div>
      <div class="ps-editor-badges">
        ${info ? caseStatusChipHtml(status) : `<tf-chip status="info" dot>${escapeHtml(t('case_new_chip'))}</tf-chip>`}
        ${info && fv(info, 'review_state') === 'pending' ? `<tf-chip status="warn">${escapeHtml(t('case_pending_chip'))}</tf-chip>` : ''}
        ${info ? `<tf-chip status="info">v${Number(fv(info, 'current_version') ?? 1)}</tf-chip>` : ''}
        ${info?.origin === 'agent' ? `<tf-chip status="accent">${sprite('sparkle')} ${escapeHtml(t('origin_agent'))}</tf-chip>` : ''}
      </div>
      <div class="ps-editor-actions">
        ${statusActions.join('')}
        ${editable ? `<tf-button variant="primary" icon="check" id="ps-case-save">${escapeHtml(t('action_save'))}</tf-button>` : ''}
        ${info && canEdit() ? `<tf-button variant="ghost" icon="trash" id="ps-case-delete" title="${escapeAttr(t('action_delete'))}"></tf-button>` : ''}
      </div>
    </div>
    ${info && fv(info, 'status_reason') ? `<div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('case_status_reason', { reason: fv(info, 'status_reason') }))}</span></div>` : ''}
    ${!editable && info ? `<div class="ps-banner-info">${sprite('lock')}<span>${escapeHtml(t('case_readonly_hint'))}</span></div>` : ''}

    <div class="ps-editor-cols">
      <div class="ps-editor-main">
        <tf-section-card title="${escapeAttr(t('case_meta_title'))}" icon="settings">
          <div class="ps-editor-meta-grid">
            <tf-select id="ps-case-priority" label="${escapeAttr(t('case_priority_label'))}" value="${escapeAttr(ed.priority)}" ${editable ? '' : 'disabled'}>
              ${PRIORITIES.map((p) => `<option value="${p}" ${p === ed.priority ? 'selected' : ''}>${escapeHtml(t(`prio_${p}`))}</option>`).join('')}
            </tf-select>
            <div class="ps-field">
              <span class="ps-field-label">${escapeHtml(t('case_tags_label'))}</span>
              <tf-tag-input id="ps-case-tags" dedupe placeholder="${escapeAttr(t('case_tags_placeholder'))}" ${editable ? '' : 'disabled'}></tf-tag-input>
              <div class="ps-field-hint">${escapeHtml(t('case_tags_hint'))}</div>
            </div>
          </div>
          <div class="ps-field">
            <span class="ps-field-label">${escapeHtml(t('case_sources_label'))}</span>
            <div class="ps-linked-sources" id="ps-case-sources">
              ${state.sources.length ? state.sources.map((src) => {
                const sid = fv(src, 'source_id');
                const on = ed.linkedSourceIds.has(sid);
                return `<tf-chip status="${on ? 'accent' : 'info'}" data-linked-source="${escapeAttr(sid)}" role="button" tabindex="0">${escapeHtml(src.name)}</tf-chip>`;
              }).join('') : `<span class="ps-field-hint">${escapeHtml(t('case_sources_empty'))}</span>`}
            </div>
          </div>
        </tf-section-card>

        <tf-section-card title="${escapeAttr(t('case_preconditions_title'))}" icon="info">
          <tf-textarea id="ps-case-preconditions" rows="3" placeholder="${escapeAttr(t('case_preconditions_placeholder'))}" ${editable ? '' : 'readonly'}></tf-textarea>
        </tf-section-card>

        <tf-section-card title="${escapeAttr(t('case_steps_title'))}" icon="list">
          <span slot="subtitle">${escapeHtml(t('case_steps_sub'))}</span>
          <div id="ps-case-steps"></div>
          ${editable ? `<tf-button variant="ghost" icon="plus" id="ps-case-add-step">${escapeHtml(t('case_add_step'))}</tf-button>` : ''}
        </tf-section-card>

        <tf-section-card title="${escapeAttr(t('case_test_data_title'))}" icon="database">
          <tf-textarea id="ps-case-test-data" rows="3" placeholder="${escapeAttr(t('case_test_data_placeholder'))}" ${editable ? '' : 'readonly'}></tf-textarea>
        </tf-section-card>

        <tf-section-card title="${escapeAttr(t('case_attachments_title'))}" icon="paperclip">
          <div id="ps-case-attachments"></div>
          ${editable ? `<tf-file-input id="ps-case-att-input" multiple label="${escapeAttr(t('attachment_dropzone'))}"></tf-file-input>` : ''}
        </tf-section-card>

        ${editable ? `
          <div class="ps-editor-savebar">
            <tf-input id="ps-case-change-note" label="${escapeAttr(t('case_change_note_label'))}" placeholder="${escapeAttr(t('case_change_note_placeholder'))}" value="${escapeAttr(ed.changeNote)}"></tf-input>
            <tf-button variant="primary" icon="check" id="ps-case-save2">${escapeHtml(t('action_save'))}</tf-button>
          </div>
        ` : ''}
      </div>

      ${ed.caseId ? `
        <div class="ps-editor-side">
          <tf-section-card title="${escapeAttr(t('case_versions_title'))}" icon="clock">
            <div id="ps-case-versions">
              ${ed.versions.length ? ed.versions.map((v) => `
                <div class="ps-version-row">
                  <tf-chip status="${Number(v.version) === ed.expectedVersion ? 'accent' : 'info'}">v${Number(v.version)}</tf-chip>
                  <div class="ps-version-main">
                    <div class="ps-version-note">${escapeHtml(fv(v, 'change_note') || t('case_version_no_note'))}</div>
                    <div class="ps-version-meta">${escapeHtml(fv(v, 'created_by_name') || '')} · ${escapeHtml(formatTimestamp(fv(v, 'created_at')))}</div>
                  </div>
                  <tf-button variant="ghost" size="sm" icon="eye" data-version-view="${Number(v.version)}" title="${escapeAttr(t('kb_preview'))}"></tf-button>
                  ${editable && Number(v.version) !== ed.expectedVersion ? `<tf-button variant="ghost" size="sm" icon="rotate" data-version-restore="${Number(v.version)}" title="${escapeAttr(t('case_restore'))}"></tf-button>` : ''}
                </div>
              `).join('') : `<div class="ps-field-hint">${escapeHtml(t('case_versions_empty'))}</div>`}
            </div>
          </tf-section-card>
          <tf-section-card title="${escapeAttr(t('case_info_title'))}" icon="info">
            <div class="ps-case-info-list">
              <div><span>${escapeHtml(t('case_info_author'))}</span><b>${escapeHtml(fv(info, 'created_by_name') || '')}</b></div>
              <div><span>${escapeHtml(t('case_info_created'))}</span><b>${escapeHtml(formatTimestamp(fv(info, 'created_at')))}</b></div>
              <div><span>${escapeHtml(t('case_info_updated'))}</span><b>${escapeHtml(formatTimestamp(fv(info, 'updated_at')))}</b></div>
            </div>
          </tf-section-card>
        </div>
      ` : ''}
    </div>
  `;

  const tagsInput = byId('ps-case-tags');
  if (tagsInput) {
    tagsInput.tags = ed.tagNames;
    tagsInput.addEventListener('change', (e) => { ed.tagNames = Array.isArray(e.detail?.tags) ? e.detail.tags : tagsInput.tags; });
  }
  const preEl = byId('ps-case-preconditions');
  if (preEl) {
    preEl.value = ed.preconditions;
    preEl.addEventListener('input', () => { ed.preconditions = String(preEl.value ?? ''); });
  }
  const tdEl = byId('ps-case-test-data');
  if (tdEl) {
    tdEl.value = ed.testData;
    tdEl.addEventListener('input', () => { ed.testData = String(tdEl.value ?? ''); });
  }
  byId('ps-case-title')?.addEventListener('input', (e) => { ed.title = String(e.target.value ?? ''); });
  byId('ps-case-priority')?.addEventListener('change', (e) => { ed.priority = e.detail?.value ?? e.target.value ?? ed.priority; });
  byId('ps-case-change-note')?.addEventListener('input', (e) => { ed.changeNote = String(e.target.value ?? ''); });
  byId('ps-case-back')?.addEventListener('click', () => { f2().editor = null; f2().view = 'cases'; renderTestsView(); });
  byId('ps-case-save')?.addEventListener('click', () => saveCaseFromEditor());
  byId('ps-case-save2')?.addEventListener('click', () => saveCaseFromEditor());
  byId('ps-case-delete')?.addEventListener('click', () => {
    if (!ed.info) return;
    deleteCaseFromEditor(ed);
  });
  byId('ps-case-add-step')?.addEventListener('click', () => {
    ed.steps.push({ action: '', expected: '' });
    renderCaseSteps(editable);
  });
  byId('ps-case-sources')?.addEventListener('click', (e) => {
    if (!editable) return;
    const chipEl = e.target.closest('[data-linked-source]');
    if (!chipEl) return;
    const sid = chipEl.dataset.linkedSource;
    if (ed.linkedSourceIds.has(sid)) ed.linkedSourceIds.delete(sid);
    else ed.linkedSourceIds.add(sid);
    chipEl.setAttribute('status', ed.linkedSourceIds.has(sid) ? 'accent' : 'info');
  });
  host.querySelectorAll('[data-status]').forEach((btn) => {
    btn.addEventListener('click', () => setCaseStatusFromEditor(btn.dataset.status, btn.hasAttribute('data-needs-reason')));
  });
  host.querySelectorAll('[data-version-view]').forEach((btn) => {
    btn.addEventListener('click', () => openVersionPreview(ed.caseId, Number(btn.dataset.versionView)));
  });
  host.querySelectorAll('[data-version-restore]').forEach((btn) => {
    btn.addEventListener('click', () => restoreCaseVersion(ed, Number(btn.dataset.versionRestore)));
  });
  byId('ps-case-att-input')?.addEventListener('change', async (e) => {
    const files = e.detail?.files;
    if (!files || !files.length || ed.uploading) return;
    ed.uploading = true;
    try {
      for (const file of Array.from(files)) {
        const att = await uploadAttachmentFile(file);
        if (!ed.attachments.some((a) => fv(a, 'sha256') === att.sha256)) ed.attachments.push(att);
      }
      renderCaseAttachments(editable);
      toast(t('attachment_uploaded'), 'success');
    } catch (err) {
      toast(`${t('attachment_upload_failed')}: ${err.message}`, 'error');
    } finally {
      ed.uploading = false;
    }
  });

  renderCaseSteps(editable);
  renderCaseAttachments(editable);
}

function renderCaseSteps(editable) {
  const ed = f2().editor;
  const host = byId('ps-case-steps');
  if (!host || !ed) return;
  host.innerHTML = ed.steps.map((step, i) => `
    <div class="ps-step-row" data-step="${i}">
      <div class="ps-step-num">${i + 1}</div>
      <div class="ps-step-fields">
        <tf-textarea rows="2" data-step-action="${i}" label="${escapeAttr(t('case_step_action'))}" ${editable ? '' : 'readonly'}></tf-textarea>
        <tf-textarea rows="2" data-step-expected="${i}" label="${escapeAttr(t('case_step_expected'))}" ${editable ? '' : 'readonly'}></tf-textarea>
      </div>
      ${editable ? `
        <div class="ps-step-tools">
          <tf-button variant="ghost" size="sm" icon="chevron-down" data-step-down="${i}" ${i === ed.steps.length - 1 ? 'disabled' : ''} title="${escapeAttr(t('case_step_down'))}"></tf-button>
          <tf-button variant="ghost" size="sm" icon="chevron-down" class="ps-rotate-180" data-step-up="${i}" ${i === 0 ? 'disabled' : ''} title="${escapeAttr(t('case_step_up'))}"></tf-button>
          <tf-button variant="ghost" size="sm" icon="trash" data-step-remove="${i}" ${ed.steps.length <= 1 ? 'disabled' : ''} title="${escapeAttr(t('case_step_remove'))}"></tf-button>
        </div>
      ` : ''}
    </div>
  `).join('');

  ed.steps.forEach((step, i) => {
    const actionEl = host.querySelector(`[data-step-action="${i}"]`);
    const expectedEl = host.querySelector(`[data-step-expected="${i}"]`);
    if (actionEl) {
      actionEl.value = step.action;
      actionEl.addEventListener('input', () => { step.action = String(actionEl.value ?? ''); });
    }
    if (expectedEl) {
      expectedEl.value = step.expected;
      expectedEl.addEventListener('input', () => { step.expected = String(expectedEl.value ?? ''); });
    }
  });
  host.querySelectorAll('[data-step-remove]').forEach((btn) => {
    btn.addEventListener('click', () => {
      ed.steps.splice(Number(btn.dataset.stepRemove), 1);
      renderCaseSteps(editable);
    });
  });
  const swap = (i, j) => {
    if (j < 0 || j >= ed.steps.length) return;
    [ed.steps[i], ed.steps[j]] = [ed.steps[j], ed.steps[i]];
    renderCaseSteps(editable);
  };
  host.querySelectorAll('[data-step-up]').forEach((btn) => {
    btn.addEventListener('click', () => swap(Number(btn.dataset.stepUp), Number(btn.dataset.stepUp) - 1));
  });
  host.querySelectorAll('[data-step-down]').forEach((btn) => {
    btn.addEventListener('click', () => swap(Number(btn.dataset.stepDown), Number(btn.dataset.stepDown) + 1));
  });
}

function renderCaseAttachments(editable) {
  const ed = f2().editor;
  const host = byId('ps-case-attachments');
  if (!host || !ed) return;
  host.innerHTML = ed.attachments.length
    ? ed.attachments.map((att, i) => attachmentRowHtml(att, i, { removable: editable })).join('')
    : `<div class="ps-field-hint">${escapeHtml(t('attachments_empty'))}</div>`;
  host.querySelectorAll('[data-att-preview]').forEach((btn) => {
    btn.addEventListener('click', () => openAttachmentPreview(ed.attachments[Number(btn.dataset.attPreview)]));
  });
  host.querySelectorAll('[data-att-remove]').forEach((btn) => {
    btn.addEventListener('click', () => {
      ed.attachments.splice(Number(btn.dataset.attRemove), 1);
      renderCaseAttachments(editable);
    });
  });
}

// Maps user-visible tag names back onto project tag ids; unknown names abort
// the save so the operator adds them in Settings first (tags are a managed
// project-level catalog, cases only reference them).
function resolveTagIds(tagNames) {
  const unknown = [];
  const ids = [];
  for (const name of tagNames) {
    const tag = f2().tags.find((x) => x.name.toLowerCase() === String(name).toLowerCase());
    if (tag) ids.push(fv(tag, 'tag_id'));
    else unknown.push(name);
  }
  return { ids, unknown };
}

async function saveCaseFromEditor() {
  const ed = f2().editor;
  if (!ed) return;
  const title = ed.title.trim();
  if (title.length < 3) {
    toast(t('err_case_title'), 'error');
    return;
  }
  const steps = ed.steps
    .map((s) => ({ action: s.action.trim(), expected: s.expected.trim() }))
    .filter((s) => s.action || s.expected);
  if (!steps.length) {
    toast(t('err_case_steps'), 'error');
    return;
  }
  const { ids: tagIds, unknown } = resolveTagIds(ed.tagNames);
  if (unknown.length) {
    toast(t('err_case_unknown_tags', { tags: unknown.join(', ') }), 'error');
    return;
  }
  const contentJson = JSON.stringify({
    preconditions: ed.preconditions,
    steps,
    test_data: ed.testData,
  });
  try {
    const resp = await ApiBinary.one('projectStudioCaseSaveRequest', {
      projectId: projectId(),
      caseId: ed.caseId,
      kind: 'manual',
      title,
      priority: ed.priority,
      contentJson,
      tagIds,
      linkedSourceIds: [...ed.linkedSourceIds],
      attachmentsJson: JSON.stringify(ed.attachments),
      expectedVersion: ed.caseId ? ed.expectedVersion : null,
      changeNote: ed.changeNote,
    });
    ed.caseId = fv(resp, 'case_id') || ed.caseId;
    ed.changeNote = '';
    toast(t('case_saved', { version: Number(resp.version ?? 1) }), 'success');
    ed.loaded = false;
    await renderTestsView();
  } catch (err) {
    // Optimistic-locking conflict: someone saved a newer version. Reload the
    // fresh detail instead of silently overwriting.
    if (/conflict|version|wersj/i.test(err.message || '')) {
      toast(t('case_conflict'), 'error');
      ed.loaded = false;
      await renderTestsView();
    } else {
      toast(`${t('case_save_failed')}: ${err.message}`, 'error');
    }
  }
}

async function setCaseStatusFromEditor(status, needsReason) {
  const ed = f2().editor;
  if (!ed?.caseId) return;
  let reason = '';
  if (needsReason) {
    reason = await openPromptWindow({ title: t('status_reason_title'), label: t('status_reason_label'), icon: 'alert' });
    if (reason == null || !reason) return;
  }
  try {
    await ApiBinary.one('projectStudioCaseStatusSetRequest', {
      projectId: projectId(), caseId: ed.caseId, status, reason,
    });
    toast(t('case_status_changed'), 'success');
    ed.loaded = false;
    await renderTestsView();
  } catch (err) {
    toast(`${t('case_status_failed')}: ${err.message}`, 'error');
  }
}

function deleteCaseFromEditor(ed) {
  openDeleteWindow({
    title: t('case_delete_title'),
    targetName: ed.title,
    targetSub: t(`case_status_${ed.info?.status || 'draft'}`),
    targetIcon: 'list',
    warning: t('case_delete_warning'),
    confirmLabel: t('delete_forever'),
    onConfirm: async () => {
      await ApiBinary.one('projectStudioCaseDeleteRequest', { projectId: projectId(), caseId: ed.caseId });
      toast(t('case_delete_ok'), 'success');
      f2().editor = null;
      f2().view = 'cases';
      await renderTestsView();
    },
  });
}

async function openVersionPreview(caseId, version) {
  let resp = null;
  try {
    resp = await ApiBinary.one('projectStudioCaseVersionGetRequest', { projectId: projectId(), caseId, version });
  } catch (err) {
    toast(`${t('case_version_failed')}: ${err.message}`, 'error');
    return;
  }
  const content = parseCaseContent(fv(resp, 'content_json'));
  const { body, foot, cleanup } = openWindow({
    title: t('case_version_title', { version }),
    subtitle: `${fv(resp, 'created_by_name') || ''} · ${formatTimestamp(fv(resp, 'created_at'))}`,
    icon: 'clock',
    width: 680,
  });
  body.innerHTML = `
    ${fv(resp, 'change_note') ? `<div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(fv(resp, 'change_note'))}</span></div>` : ''}
    ${content.preconditions ? `
      <div class="ps-field"><span class="ps-field-label">${escapeHtml(t('case_preconditions_title'))}</span>
      <div class="ps-version-block">${escapeHtml(content.preconditions)}</div></div>` : ''}
    <div class="ps-field"><span class="ps-field-label">${escapeHtml(t('case_steps_title'))}</span>
      ${content.steps.map((s, i) => `
        <div class="ps-version-step">
          <div class="ps-step-num">${i + 1}</div>
          <div>
            <div><b>${escapeHtml(t('case_step_action'))}:</b> ${escapeHtml(s.action)}</div>
            <div><b>${escapeHtml(t('case_step_expected'))}:</b> ${escapeHtml(s.expected)}</div>
          </div>
        </div>
      `).join('')}
    </div>
    ${content.testData ? `
      <div class="ps-field"><span class="ps-field-label">${escapeHtml(t('case_test_data_title'))}</span>
      <div class="ps-version-block">${escapeHtml(content.testData)}</div></div>` : ''}
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="close-version">${escapeHtml(t('action_close'))}</tf-button>
    </div>
  `;
  foot.addEventListener('click', (e) => {
    if (e.target.closest('[data-action="close-version"]')) cleanup();
  });
}

async function restoreCaseVersion(ed, version) {
  const ok = await TfWindow.confirm({
    title: t('case_restore_title'),
    message: t('case_restore_message', { version }),
    confirmLabel: t('case_restore'),
    cancelLabel: t('action_cancel'),
  });
  if (!ok) return;
  try {
    await ApiBinary.one('projectStudioCaseRestoreVersionRequest', {
      projectId: projectId(), caseId: ed.caseId, version, expectedVersion: ed.expectedVersion,
    });
    toast(t('case_restored'), 'success');
    ed.loaded = false;
    await renderTestsView();
  } catch (err) {
    toast(`${t('case_restore_failed')}: ${err.message}`, 'error');
  }
}

// =============================================================================
// T04 — generation wizard (3 steps; F2 supports 'manual' only)
// =============================================================================

function openGenerationWindow() {
  const { body, foot, cleanup } = openWindow({
    title: t('gen_win_title'),
    subtitle: t('gen_win_sub'),
    icon: 'sparkle',
    width: 720,
  });

  const gw = {
    step: 1,
    kind: 'manual',
    count: '',
    instructions: '',
    sourceIds: new Set(),
    agentId: '',
    busy: false,
  };

  body.innerHTML = `
    <div class="ps-stepper">
      <div class="ps-step" data-step-pill="1"><span class="ps-step-n">1</span>${escapeHtml(t('gen_step1'))}</div>
      <div class="ps-step-line" data-step-line="1"></div>
      <div class="ps-step" data-step-pill="2"><span class="ps-step-n">2</span>${escapeHtml(t('gen_step2'))}</div>
      <div class="ps-step-line" data-step-line="2"></div>
      <div class="ps-step" data-step-pill="3"><span class="ps-step-n">3</span>${escapeHtml(t('gen_step3'))}</div>
    </div>

    <div data-step-panel="1">
      <div class="ps-field">
        <span class="ps-field-label">${escapeHtml(t('gen_kind_label'))}</span>
        <div class="ps-choice-grid" data-gen-kinds>
          ${CASE_KINDS.map((k) => `
            <div class="ps-choice-card ${k === 'manual' ? 'is-selected' : 'is-disabled'}" data-gen-kind="${k}" role="button" tabindex="${k === 'manual' ? '0' : '-1'}">
              <div class="ps-cc-ico">${sprite(k === 'manual' ? 'list' : 'code')}</div>
              <div>
                <div class="ps-cc-name">${escapeHtml(t(`case_kind_${k}`))}${k === 'manual' ? '' : ` <tf-chip status="info">${escapeHtml(t('kind_soon'))}</tf-chip>`}</div>
              </div>
            </div>
          `).join('')}
        </div>
      </div>
      <div class="ps-field" style="margin-bottom:12px;">
        <tf-input id="ps-gen-count" type="number" min="0" max="30" label="${escapeAttr(t('gen_count_label'))}"
          placeholder="${escapeAttr(t('gen_count_auto'))}" hint="${escapeAttr(t('gen_count_hint'))}"></tf-input>
      </div>
      <div class="ps-field">
        <tf-textarea id="ps-gen-instructions" rows="4" label="${escapeAttr(t('gen_instructions_label'))}"
          hint="${escapeAttr(t('gen_instructions_hint'))}"></tf-textarea>
      </div>
    </div>

    <div data-step-panel="2" hidden>
      <div class="ps-field-hint" style="margin-bottom:10px;">${escapeHtml(t('gen_sources_hint'))}</div>
      <div data-gen-sources></div>
    </div>

    <div data-step-panel="3" hidden>
      <div class="ps-field" style="margin-bottom:12px;">
        <tf-select id="ps-gen-agent" label="${escapeAttr(t('gen_agent_label'))}" value=""></tf-select>
        <div class="ps-field-hint">${escapeHtml(t('gen_agent_hint'))}</div>
      </div>
      <div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('gen_review_hint'))}</span></div>
      <div data-gen-summary></div>
    </div>

    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left">
      <tf-button variant="ghost" icon="chevron-left" data-action="back">${escapeHtml(t('wizard_back'))}</tf-button>
    </div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="primary" data-action="next"></tf-button>
    </div>
  `;

  const showError = (msg) => {
    const el = body.querySelector('[data-form-error]');
    if (el) { el.hidden = !msg; el.textContent = msg || ''; }
  };

  const renderSources = () => {
    const hostEl = body.querySelector('[data-gen-sources]');
    if (!hostEl) return;
    if (!state.sources.length) {
      hostEl.innerHTML = `<tf-empty-state icon="database" title="${escapeAttr(t('gen_sources_empty'))}"></tf-empty-state>`;
      return;
    }
    hostEl.innerHTML = state.sources.map((src) => {
      const sid = fv(src, 'source_id');
      const ready = src.status === 'ready';
      return `
        <div class="ps-gen-source-row ${ready ? '' : 'is-disabled'}" title="${ready ? '' : escapeAttr(t(`source_status_${src.status}`))}">
          <tf-checkbox data-gen-source="${escapeAttr(sid)}" ${gw.sourceIds.has(sid) ? 'checked' : ''} ${ready ? '' : 'disabled'}></tf-checkbox>
          <div class="ps-source-ico">${sprite(SOURCE_KIND_ICON[src.kind] || 'file-text')}</div>
          <div class="ps-gen-source-main">
            <div class="ps-gen-source-name">${escapeHtml(src.name)}</div>
            <div class="ps-gen-source-meta">${escapeHtml(t(`kind_${src.kind}`))} · ${escapeHtml(t('source_chunks_count', { count: fv(src, 'chunk_count') ?? 0 }))}</div>
          </div>
          <tf-chip status="${SOURCE_STATUS_CHIP[src.status] || 'info'}" dot>${escapeHtml(t(`source_status_${src.status}`))}</tf-chip>
        </div>
      `;
    }).join('');
    hostEl.querySelectorAll('[data-gen-source]').forEach((cb) => {
      cb.addEventListener('change', () => {
        const sid = cb.dataset.genSource;
        if (cb.checked) gw.sourceIds.add(sid);
        else gw.sourceIds.delete(sid);
      });
    });
  };

  const renderAgentSelect = () => {
    const sel = body.querySelector('#ps-gen-agent');
    if (!sel) return;
    // The select is already connected — light-DOM options were consumed at
    // build time, so async option lists must go through setOptions().
    sel.setOptions([
      { value: '', label: t('gen_agent_default') },
      ...state.agentOptions.map((a) => ({ value: a.id, label: a.name })),
    ], gw.agentId || '');
    sel.addEventListener('change', (e) => { gw.agentId = e.detail?.value ?? sel.value ?? ''; });
  };

  const renderSummary = () => {
    const hostEl = body.querySelector('[data-gen-summary]');
    if (!hostEl) return;
    const names = state.sources
      .filter((src) => gw.sourceIds.has(fv(src, 'source_id')))
      .map((src) => src.name);
    hostEl.innerHTML = `
      <div class="ps-gen-summary">
        <div><span>${escapeHtml(t('gen_summary_kind'))}</span><b>${escapeHtml(t(`case_kind_${gw.kind}`))}</b></div>
        <div><span>${escapeHtml(t('gen_summary_count'))}</span><b>${escapeHtml(gw.count || t('gen_count_auto'))}</b></div>
        <div><span>${escapeHtml(t('gen_summary_sources'))}</span><b>${escapeHtml(names.join(', ') || '—')}</b></div>
      </div>
    `;
  };

  const setStep = (step) => {
    gw.step = step;
    body.querySelectorAll('[data-step-panel]').forEach((p) => { p.hidden = Number(p.dataset.stepPanel) !== step; });
    body.querySelectorAll('[data-step-pill]').forEach((pill) => {
      const n = Number(pill.dataset.stepPill);
      pill.classList.toggle('is-active', n === step);
      pill.classList.toggle('is-done', n < step);
    });
    body.querySelectorAll('[data-step-line]').forEach((line) => {
      line.classList.toggle('is-done', Number(line.dataset.stepLine) < step);
    });
    const backBtn = foot.querySelector('[data-action="back"]');
    if (backBtn) backBtn.style.visibility = step > 1 ? 'visible' : 'hidden';
    const nextBtn = foot.querySelector('[data-action="next"]');
    if (nextBtn) {
      nextBtn.setAttribute('icon', step < 3 ? 'chevron-right' : 'sparkle');
      nextBtn.setAttribute('label', step < 3 ? t('wizard_next') : t('gen_start'));
    }
    if (step === 3) renderSummary();
    showError(null);
  };

  const start = async () => {
    if (gw.busy) return;
    gw.busy = true;
    try {
      const resp = await ApiBinary.one('projectStudioGenerationStartRequest', {
        projectId: projectId(),
        kind: gw.kind,
        sourceIds: [...gw.sourceIds],
        requestedCount: Number(gw.count || 0),
        instructions: gw.instructions,
        agentId: gw.agentId || null,
      });
      toast(t('gen_started'), 'success');
      cleanup();
      const genId = fv(resp, 'gen_id');
      f2().view = 'generations';
      await renderTestsView();
      if (genId) await openGenDetail(genId);
    } catch (err) {
      gw.busy = false;
      showError(`${t('gen_start_failed')}: ${err.message}`);
    }
  };

  foot.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    if (btn.dataset.action === 'back') { if (gw.step > 1) setStep(gw.step - 1); return; }
    if (gw.step === 1) {
      gw.count = String(body.querySelector('#ps-gen-count')?.value ?? '').trim();
      gw.instructions = String(body.querySelector('#ps-gen-instructions')?.value ?? '').trim();
      setStep(2);
    } else if (gw.step === 2) {
      if (!gw.sourceIds.size) { showError(t('err_gen_sources')); return; }
      setStep(3);
    } else {
      start();
    }
  });

  // Ready sources + org agents load lazily so opening the window stays instant.
  (async () => {
    if (!state.sources.length) {
      try { await loadSources(); } catch { /* source list stays empty */ }
    }
    renderSources();
    if (!state.agentOptions.length) {
      try {
        const resp = await ApiBinary.one('agentsListRequest', {});
        const rows = JSON.parse(fv(resp, 'agents_json') ?? '[]');
        state.agentOptions = Array.isArray(rows)
          ? rows.filter((a) => a.is_enabled).map((a) => ({ id: a.id, name: a.display_name || a.name }))
          : [];
      } catch { /* select keeps only the default option */ }
    }
    renderAgentSelect();
  })();

  setStep(1);
}

// =============================================================================
// T05 — generations list + detail (live for the initiator, polling for others)
// =============================================================================

async function renderGenerationsView() {
  const host = byId('ps-tests-host');
  if (!host) return;
  const s = f2();
  try {
    const resp = await ApiBinary.one('projectStudioGenerationsListRequest', { projectId: projectId() });
    s.gens = Array.isArray(resp.generations) ? resp.generations : [];
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('gens_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || s.view !== 'generations') return;

  host.innerHTML = `
    <div class="ps-tests-toolbar">
      <span class="ps-toolbar-spacer"></span>
      ${canEdit() ? `<tf-button variant="primary" icon="sparkle" id="ps-gens-new">${escapeHtml(t('cases_generate'))}</tf-button>` : ''}
    </div>
    <div id="ps-gens-table-host">
      ${s.gens.length ? '' : `<tf-empty-state icon="sparkle" title="${escapeAttr(t('gens_empty'))}"></tf-empty-state>`}
    </div>
  `;
  byId('ps-gens-new')?.addEventListener('click', () => openGenerationWindow());
  if (!s.gens.length) return;

  const tableHost = byId('ps-gens-table-host');
  tableHost.innerHTML = `
    <tf-table id="ps-gens-table">
      <tf-column key="kind" label="${escapeAttr(t('gens_col_kind'))}" renderer="chip"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('gens_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="agent" label="${escapeAttr(t('gens_col_agent'))}"></tf-column>
      <tf-column key="progress" label="${escapeAttr(t('gens_col_progress'))}"></tf-column>
      <tf-column key="startedBy" label="${escapeAttr(t('gens_col_started_by'))}"></tf-column>
      <tf-column key="startedAt" label="${escapeAttr(t('gens_col_started_at'))}"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-gens-table');
  table.rows = s.gens.map((g) => ({
    _id: fv(g, 'gen_id'),
    _row: g,
    kind: chipCell('info', t(`case_kind_${g.kind}`)),
    status: chipCell(GEN_STATUS_CHIP[g.status], t(`gen_status_${g.status}`)),
    agent: fv(g, 'agent_name') || '—',
    progress: t('gens_progress', {
      generated: Number(fv(g, 'cases_generated') ?? 0),
      accepted: Number(fv(g, 'cases_accepted') ?? 0),
      rejected: Number(fv(g, 'cases_rejected') ?? 0),
    }),
    startedBy: fv(g, 'started_by_name') || '',
    startedAt: formatTimestamp(fv(g, 'started_at')),
  }));
  table.rowActions = (row) => {
    const wrap = document.createElement('div');
    wrap.className = 'ps-file-actions';
    const open = document.createElement('tf-button');
    open.setAttribute('variant', 'ghost');
    open.setAttribute('size', 'sm');
    open.setAttribute('icon', 'external-link');
    open.setAttribute('title', t('gens_open'));
    open.addEventListener('click', (e) => { e.stopPropagation(); openGenDetail(row._id); });
    wrap.appendChild(open);
    const status = row._row.status;
    if (canManage() && status !== 'running' && status !== 'review') {
      const del = document.createElement('tf-button');
      del.setAttribute('variant', 'ghost');
      del.setAttribute('size', 'sm');
      del.setAttribute('icon', 'trash');
      del.setAttribute('title', t('action_delete'));
      del.addEventListener('click', (e) => { e.stopPropagation(); deleteGeneration(row._id); });
      wrap.appendChild(del);
    }
    return wrap;
  };
  table.addEventListener('row-click', (e) => {
    const genId = e.detail?.row?._id;
    if (genId) openGenDetail(genId);
  });
}

async function deleteGeneration(genId) {
  const ok = await TfWindow.confirm({
    title: t('gen_delete_title'),
    message: t('gen_delete_message'),
    confirmLabel: t('action_delete'),
    cancelLabel: t('action_cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await ApiBinary.one('projectStudioGenerationDeleteRequest', { projectId: projectId(), genId });
    toast(t('gen_deleted'), 'success');
    if (f2().view === 'gen-detail') { f2().view = 'generations'; }
    await renderTestsView();
  } catch (err) {
    toast(`${t('gen_delete_failed')}: ${err.message}`, 'error');
  }
}

async function openGenDetail(genId) {
  stopTestsLive();
  f2().genDetail = { genId, run: null, pendingCases: [], selected: new Set(), loaded: false };
  f2().view = 'gen-detail';
  await renderTestsView();
}

async function loadGenDetail() {
  const gd = f2().genDetail;
  const resp = await ApiBinary.one('projectStudioGenerationGetRequest', { projectId: projectId(), genId: gd.genId });
  gd.run = resp.run || null;
  gd.pendingCases = Array.isArray(fv(resp, 'pending_cases')) ? fv(resp, 'pending_cases') : [];
  gd.loaded = true;
}

async function renderGenDetailView() {
  const host = byId('ps-tests-host');
  const s = f2();
  const gd = s.genDetail;
  if (!host || !gd) { s.view = 'generations'; return renderGenerationsView(); }
  try {
    await loadGenDetail();
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('gen_load_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || s.view !== 'gen-detail') return;
  const run = gd.run;
  if (!run) { s.view = 'generations'; return renderGenerationsView(); }
  const running = run.status === 'running';
  const initiator = isMe(fv(run, 'started_by'));
  const maxCases = Math.max(1, Number(fv(run, 'max_cases') ?? 1));
  const generated = Number(fv(run, 'cases_generated') ?? 0);
  const pct = Math.min(100, Math.round((generated / maxCases) * 100));

  host.innerHTML = `
    <div class="ps-editor-head">
      <tf-button variant="ghost" icon="chevron-left" id="ps-gen-back">${escapeHtml(t('back_to_gens'))}</tf-button>
      <div class="ps-editor-title-static">
        <div class="ps-detail-name">${escapeHtml(t('gen_detail_title'))}</div>
        <div class="ps-detail-sub">${escapeHtml(fv(run, 'agent_name') || '')} · ${escapeHtml(fv(run, 'started_by_name') || '')} · ${escapeHtml(formatTimestamp(fv(run, 'started_at')))}</div>
      </div>
      <div class="ps-editor-badges">
        <tf-chip status="${GEN_STATUS_CHIP[run.status] || 'info'}" dot>${escapeHtml(t(`gen_status_${run.status}`))}</tf-chip>
        <tf-chip status="info">${escapeHtml(t('gens_progress', { generated, accepted: Number(fv(run, 'cases_accepted') ?? 0), rejected: Number(fv(run, 'cases_rejected') ?? 0) }))}</tf-chip>
      </div>
      <div class="ps-editor-actions">
        ${running && (initiator || canManage()) ? `<tf-button variant="ghost" icon="ban" id="ps-gen-cancel">${escapeHtml(t('gen_cancel'))}</tf-button>` : ''}
        ${!running && run.status !== 'review' && canManage() ? `<tf-button variant="ghost" icon="trash" id="ps-gen-delete" title="${escapeAttr(t('action_delete'))}"></tf-button>` : ''}
      </div>
    </div>
    ${run.error ? `<div class="ps-form-error">${escapeHtml(run.error)}</div>` : ''}
    ${run.instructions ? `<div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('gen_instructions_label'))}: ${escapeHtml(run.instructions)}</span></div>` : ''}

    ${running ? `
      <tf-section-card title="${escapeAttr(t('gen_live_title'))}" icon="sparkle">
        <span slot="subtitle">${escapeHtml(initiator ? t('gen_live_sub_initiator') : t('gen_live_sub_other'))}</span>
        <div class="ps-gen-progress">
          <tf-progress-bar id="ps-gen-progressbar" value="${pct}"></tf-progress-bar>
          <span id="ps-gen-progress-label">${escapeHtml(t('gen_progress_label', { generated, max: maxCases }))}</span>
        </div>
        ${initiator ? `
          <div id="ps-gen-widget-host"></div>
          <div class="ps-gen-timeline" id="ps-gen-timeline"></div>
        ` : ''}
      </tf-section-card>
    ` : ''}

    ${run.status === 'review' ? `
      <tf-section-card title="${escapeAttr(t('gen_review_title'))}" icon="eye">
        <span slot="subtitle">${escapeHtml(t('gen_review_sub', { count: gd.pendingCases.length }))}</span>
        <span slot="actions">
          ${canEdit() ? `
            <tf-button variant="ghost" size="sm" id="ps-gen-select-all">${escapeHtml(t('gen_select_all'))}</tf-button>
            <tf-button variant="primary" size="sm" icon="check" id="ps-gen-accept">${escapeHtml(t('gen_accept_selected'))}</tf-button>
            <tf-button variant="danger-solid" size="sm" icon="x" id="ps-gen-reject">${escapeHtml(t('gen_reject_selected'))}</tf-button>
          ` : ''}
        </span>
        <div id="ps-gen-pending"></div>
      </tf-section-card>
    ` : ''}
  `;

  byId('ps-gen-back')?.addEventListener('click', () => { stopTestsLive(); s.view = 'generations'; renderTestsView(); });
  byId('ps-gen-cancel')?.addEventListener('click', async () => {
    try {
      await ApiBinary.one('projectStudioGenerationCancelRequest', { projectId: projectId(), genId: gd.genId });
      toast(t('gen_cancelled'), 'success');
      stopTestsLive();
      await renderTestsView();
    } catch (err) {
      toast(`${t('gen_cancel_failed')}: ${err.message}`, 'error');
    }
  });
  byId('ps-gen-delete')?.addEventListener('click', () => deleteGeneration(gd.genId));
  byId('ps-gen-select-all')?.addEventListener('click', () => {
    const all = gd.selected.size === gd.pendingCases.length;
    gd.selected = all ? new Set() : new Set(gd.pendingCases.map((c) => fv(c, 'case_id')));
    renderGenPendingList();
  });
  byId('ps-gen-accept')?.addEventListener('click', () => reviewGeneration('accept'));
  byId('ps-gen-reject')?.addEventListener('click', () => reviewGeneration('reject'));

  if (run.status === 'review') renderGenPendingList();
  if (running) startGenLive(run, initiator);
}

function renderGenPendingList() {
  const gd = f2().genDetail;
  const host = byId('ps-gen-pending');
  if (!host || !gd) return;
  if (!gd.pendingCases.length) {
    host.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('gen_pending_empty'))}</div>`;
    return;
  }
  host.innerHTML = gd.pendingCases.map((c) => {
    const caseId = fv(c, 'case_id');
    return `
      <div class="ps-gen-pending-row">
        <tf-checkbox data-gen-case="${escapeAttr(caseId)}" ${gd.selected.has(caseId) ? 'checked' : ''}></tf-checkbox>
        <div class="ps-gen-pending-main">
          <div class="ps-gen-pending-title">${escapeHtml(c.title)}</div>
          <div class="ps-gen-pending-meta">
            <tf-chip status="${PRIORITY_CHIP[c.priority] || 'info'}">${escapeHtml(t(`prio_${c.priority}`))}</tf-chip>
            <tf-chip status="info">${escapeHtml(t(`case_kind_${c.kind}`))}</tf-chip>
            ${(fv(c, 'tag_ids') || []).map(tagNameById).filter(Boolean).map((name) => `<tf-chip status="info">${escapeHtml(name)}</tf-chip>`).join('')}
          </div>
        </div>
      </div>
    `;
  }).join('');
  host.querySelectorAll('[data-gen-case]').forEach((cb) => {
    cb.addEventListener('change', () => {
      const id = cb.dataset.genCase;
      if (cb.checked) gd.selected.add(id);
      else gd.selected.delete(id);
    });
  });
}

async function reviewGeneration(mode) {
  const gd = f2().genDetail;
  if (!gd) return;
  const ids = [...gd.selected];
  if (!ids.length) {
    toast(t('gen_review_none_selected'), 'error');
    return;
  }
  try {
    const resp = await ApiBinary.one('projectStudioGenerationReviewRequest', {
      projectId: projectId(),
      genId: gd.genId,
      acceptCaseIds: mode === 'accept' ? ids : [],
      rejectCaseIds: mode === 'reject' ? ids : [],
    });
    toast(t('gen_review_ok', { accepted: Number(resp.accepted ?? 0), rejected: Number(resp.rejected ?? 0) }), 'success');
    gd.selected = new Set();
    await renderTestsView();
  } catch (err) {
    toast(`${t('gen_review_failed')}: ${err.message}`, 'error');
  }
}

// Live view: the initiator subscribes to the agent-run event stream (run-scope
// ACL admits only the run owner — run_events.rs:203); everyone else relies on
// the 3 s GenerationGet poll, which stays the source of truth for both.
function startGenLive(run, initiator) {
  const s = f2();
  const gd = s.genDetail;
  const genId = gd.genId;

  if (initiator && fv(run, 'agent_run_id')) {
    const widgetHost = byId('ps-gen-widget-host');
    if (widgetHost) {
      const widget = document.createElement('tf-agent-activity');
      widget.labels = activityLabels();
      widgetHost.replaceChildren(widget);
      s.genWidget = widget;
    }
    s.genSteps = [];
    ApiBinary.subscribe(
      'agentRunEventsSubscribeRequest',
      { scopeKind: 'run', scopeId: fv(run, 'agent_run_id') },
      {
        onChunk: (body) => {
          if (!body || body.variant !== 'AgentRunEvent') return;
          if (s.genDetail !== gd || s.view !== 'gen-detail') return;
          s.genWidget?.applyEvent(body);
          const step = TfAgentActivity.stepsFromEvents([body], activityLabels())[0];
          step.ts = new Date().toLocaleTimeString();
          s.genSteps.push(step);
          const tl = byId('ps-gen-timeline');
          if (tl) {
            tl.innerHTML = TfAgentActivity.renderTimeline(s.genSteps, activityLabels());
            tl.scrollTop = tl.scrollHeight;
          }
        },
        onError: () => { /* the poll below is the source of truth */ },
        onEnd: () => { /* terminal state is confirmed by the poll */ },
      },
    ).then((unsub) => {
      if (s.genDetail !== gd || s.view !== 'gen-detail') { unsub(); return; }
      s.genUnsub = unsub;
    }).catch(() => { /* stream is optional; polling still tracks the run */ });
  }

  s.genPollTimer = setInterval(async () => {
    if (s.genDetail !== gd || s.view !== 'gen-detail' || state.tab !== 'tests') {
      stopTestsLive();
      return;
    }
    let fresh = null;
    try {
      const resp = await ApiBinary.one('projectStudioGenerationGetRequest', { projectId: projectId(), genId });
      fresh = resp.run;
    } catch {
      return;
    }
    if (!fresh) return;
    const generated = Number(fv(fresh, 'cases_generated') ?? 0);
    const maxCases = Math.max(1, Number(fv(fresh, 'max_cases') ?? 1));
    byId('ps-gen-progressbar')?.setAttribute('value', String(Math.min(100, Math.round((generated / maxCases) * 100))));
    const label = byId('ps-gen-progress-label');
    if (label) label.textContent = t('gen_progress_label', { generated, max: maxCases });
    if (fresh.status !== 'running') {
      stopTestsLive();
      await renderTestsView();
    }
  }, GEN_POLL_MS);
}

// =============================================================================
// T06 — suites (list + two-pane editor with explicit ordering)
// =============================================================================

async function renderSuitesView() {
  const host = byId('ps-tests-host');
  if (!host) return;
  const s = f2();
  try {
    const resp = await ApiBinary.one('projectStudioSuitesListRequest', { projectId: projectId() });
    s.suites = Array.isArray(resp.suites) ? resp.suites : [];
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('suites_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || s.view !== 'suites') return;

  host.innerHTML = `
    <div class="ps-tests-toolbar">
      <span class="ps-toolbar-spacer"></span>
      ${canEdit() ? `<tf-button variant="primary" icon="plus" id="ps-suites-new">${escapeHtml(t('suites_new'))}</tf-button>` : ''}
    </div>
    <div id="ps-suites-table-host">
      ${s.suites.length ? '' : `<tf-empty-state icon="grid-2x2" title="${escapeAttr(t('suites_empty'))}"></tf-empty-state>`}
    </div>
  `;
  byId('ps-suites-new')?.addEventListener('click', () => openSuiteEditor(null));
  if (!s.suites.length) return;

  byId('ps-suites-table-host').innerHTML = `
    <tf-table id="ps-suites-table">
      <tf-column key="name" label="${escapeAttr(t('suites_col_name'))}"></tf-column>
      <tf-column key="cases" label="${escapeAttr(t('suites_col_cases'))}" renderer="num"></tf-column>
      <tf-column key="deprecated" label="${escapeAttr(t('suites_col_health'))}" renderer="chip"></tf-column>
      <tf-column key="lastRun" label="${escapeAttr(t('suites_col_last_run'))}"></tf-column>
      <tf-column key="updated" label="${escapeAttr(t('suites_col_updated'))}"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-suites-table');
  table.rows = s.suites.map((suite) => {
    const lastRun = fv(suite, 'last_run');
    return {
      _id: fv(suite, 'suite_id'),
      _row: suite,
      name: suite.name,
      cases: Number(fv(suite, 'case_count') ?? 0),
      deprecated: fv(suite, 'has_deprecated')
        ? chipCell('warn', t('suites_has_deprecated'))
        : chipCell('ok', t('suites_ok')),
      lastRun: lastRun
        ? `#${fv(lastRun, 'run_no')} · ${t(`run_status_${lastRun.status}`)}`
        : '—',
      updated: formatTimestamp(fv(suite, 'updated_at')),
    };
  });
  table.rowActions = (row) => {
    const wrap = document.createElement('div');
    wrap.className = 'ps-file-actions';
    const mk = (icon, title, handler) => {
      const btn = document.createElement('tf-button');
      btn.setAttribute('variant', 'ghost');
      btn.setAttribute('size', 'sm');
      btn.setAttribute('icon', icon);
      btn.setAttribute('title', title);
      btn.addEventListener('click', (e) => { e.stopPropagation(); handler(); });
      wrap.appendChild(btn);
    };
    if (canEdit()) {
      mk('edit', t('action_edit'), () => openSuiteEditor(row._id));
      mk('play', t('suites_run'), () => openRunWindow({ suiteId: row._id }));
      mk('trash', t('action_delete'), () => deleteSuite(row._row));
    } else {
      mk('eye', t('suites_open'), () => openSuiteEditor(row._id));
    }
    return wrap;
  };
  table.addEventListener('row-click', (e) => {
    const suiteId = e.detail?.row?._id;
    if (suiteId) openSuiteEditor(suiteId);
  });
}

function deleteSuite(suite) {
  openDeleteWindow({
    title: t('suite_delete_title'),
    targetName: suite.name,
    targetSub: t('suites_col_cases_sub', { count: Number(fv(suite, 'case_count') ?? 0) }),
    targetIcon: 'grid-2x2',
    warning: t('suite_delete_warning'),
    confirmLabel: t('delete_forever'),
    onConfirm: async () => {
      await ApiBinary.one('projectStudioSuiteDeleteRequest', { projectId: projectId(), suiteId: fv(suite, 'suite_id') });
      toast(t('suite_delete_ok'), 'success');
      await renderSuitesView();
    },
  });
}

async function openSuiteEditor(suiteId) {
  f2().suiteEditor = {
    suiteId: suiteId || null,
    loaded: !suiteId,
    name: '',
    description: '',
    // Ordered [{ caseId, title, priority, status }] — array order = positions.
    cases: [],
    pool: [],
    poolFilter: '',
  };
  f2().view = 'suite-editor';
  await renderTestsView();
}

async function renderSuiteEditor() {
  const host = byId('ps-tests-host');
  const se = f2().suiteEditor;
  if (!host || !se) { f2().view = 'suites'; return renderSuitesView(); }
  try {
    if (!se.loaded) {
      const resp = await ApiBinary.one('projectStudioSuiteGetRequest', { projectId: projectId(), suiteId: se.suiteId });
      const suite = resp.suite || {};
      se.name = suite.name || '';
      se.description = suite.description || '';
      se.cases = (Array.isArray(resp.cases) ? resp.cases : [])
        .slice()
        .sort((a, b) => Number(a.position ?? 0) - Number(b.position ?? 0))
        .map((c) => ({ caseId: fv(c, 'case_id'), title: c.title, priority: c.priority, status: c.status }));
      se.loaded = true;
    }
    if (!se.pool.length) {
      const resp = await ApiBinary.one('projectStudioCasesListRequest', {
        projectId: projectId(), status: 'approved', offset: 0, limit: 200,
      });
      se.pool = Array.isArray(resp.cases) ? resp.cases : [];
    }
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('suite_load_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || f2().view !== 'suite-editor') return;
  const editable = canEdit();

  host.innerHTML = `
    <div class="ps-editor-head">
      <tf-button variant="ghost" icon="chevron-left" id="ps-suite-back">${escapeHtml(t('back_to_suites'))}</tf-button>
      <div class="ps-editor-title">
        <tf-input id="ps-suite-name" label="${escapeAttr(t('suite_name_label'))}" value="${escapeAttr(se.name)}" ${editable ? '' : 'readonly'}></tf-input>
      </div>
      <div class="ps-editor-actions">
        ${editable ? `<tf-button variant="primary" icon="check" id="ps-suite-save">${escapeHtml(t('action_save'))}</tf-button>` : ''}
      </div>
    </div>
    <div class="ps-field" style="margin-bottom:12px;">
      <tf-input id="ps-suite-desc" label="${escapeAttr(t('suite_desc_label'))}" value="${escapeAttr(se.description)}" ${editable ? '' : 'readonly'}></tf-input>
    </div>
    <div class="ps-two-pane">
      <tf-section-card title="${escapeAttr(t('suite_pool_title'))}" icon="list">
        <span slot="subtitle">${escapeHtml(t('suite_pool_sub'))}</span>
        <tf-searchbox id="ps-suite-pool-filter" placeholder="${escapeAttr(t('suite_pool_filter'))}" debounce="200" value="${escapeAttr(se.poolFilter)}"></tf-searchbox>
        <div class="ps-pane-list" id="ps-suite-pool"></div>
      </tf-section-card>
      <tf-section-card title="${escapeAttr(t('suite_members_title'))}" icon="grid-2x2">
        <span slot="subtitle">${escapeHtml(t('suite_members_sub', { count: se.cases.length }))}</span>
        <div class="ps-pane-list" id="ps-suite-members"></div>
      </tf-section-card>
    </div>
  `;

  byId('ps-suite-back')?.addEventListener('click', () => { f2().suiteEditor = null; f2().view = 'suites'; renderTestsView(); });
  byId('ps-suite-name')?.addEventListener('input', (e) => { se.name = String(e.target.value ?? ''); });
  byId('ps-suite-desc')?.addEventListener('input', (e) => { se.description = String(e.target.value ?? ''); });
  byId('ps-suite-pool-filter')?.addEventListener('search', (e) => {
    se.poolFilter = String(e.detail?.value ?? '');
    renderSuitePool(editable);
  });
  byId('ps-suite-save')?.addEventListener('click', async () => {
    const name = se.name.trim();
    if (name.length < 2) { toast(t('err_suite_name'), 'error'); return; }
    try {
      await ApiBinary.one('projectStudioSuiteSaveRequest', {
        projectId: projectId(),
        suiteId: se.suiteId,
        name,
        description: se.description.trim(),
        caseIds: se.cases.map((c) => c.caseId),
      });
      toast(t('suite_saved'), 'success');
      f2().suiteEditor = null;
      f2().view = 'suites';
      await renderTestsView();
    } catch (err) {
      toast(`${t('suite_save_failed')}: ${err.message}`, 'error');
    }
  });

  renderSuitePool(editable);
  renderSuiteMembers(editable);
}

function renderSuitePool(editable) {
  const se = f2().suiteEditor;
  const host = byId('ps-suite-pool');
  if (!host || !se) return;
  const chosen = new Set(se.cases.map((c) => c.caseId));
  const query = se.poolFilter.trim().toLowerCase();
  const rows = se.pool.filter((c) => {
    if (chosen.has(fv(c, 'case_id'))) return false;
    if (query && !String(c.title).toLowerCase().includes(query)) return false;
    return true;
  });
  if (!rows.length) {
    host.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('suite_pool_empty'))}</div>`;
    return;
  }
  host.innerHTML = rows.map((c) => `
    <div class="ps-pane-row">
      <div class="ps-pane-row-main">
        <div class="ps-pane-row-title">${escapeHtml(c.title)}</div>
        <tf-chip status="${PRIORITY_CHIP[c.priority] || 'info'}">${escapeHtml(t(`prio_${c.priority}`))}</tf-chip>
      </div>
      ${editable ? `<tf-button variant="ghost" size="sm" icon="plus" data-pool-add="${escapeAttr(fv(c, 'case_id'))}" title="${escapeAttr(t('suite_add_case'))}"></tf-button>` : ''}
    </div>
  `).join('');
  host.querySelectorAll('[data-pool-add]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const c = se.pool.find((x) => fv(x, 'case_id') === btn.dataset.poolAdd);
      if (!c) return;
      se.cases.push({ caseId: fv(c, 'case_id'), title: c.title, priority: c.priority, status: c.status });
      renderSuitePool(editable);
      renderSuiteMembers(editable);
    });
  });
}

function renderSuiteMembers(editable) {
  const se = f2().suiteEditor;
  const host = byId('ps-suite-members');
  if (!host || !se) return;
  if (!se.cases.length) {
    host.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('suite_members_empty'))}</div>`;
    return;
  }
  host.innerHTML = se.cases.map((c, i) => `
    <div class="ps-pane-row">
      <div class="ps-step-num">${i + 1}</div>
      <div class="ps-pane-row-main">
        <div class="ps-pane-row-title">${escapeHtml(c.title)}</div>
        ${c.status === 'deprecated' ? `<tf-chip status="warn">${escapeHtml(t('case_status_deprecated'))}</tf-chip>` : ''}
      </div>
      ${editable ? `
        <tf-button variant="ghost" size="sm" icon="chevron-down" class="ps-rotate-180" data-member-up="${i}" ${i === 0 ? 'disabled' : ''} title="${escapeAttr(t('case_step_up'))}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="chevron-down" data-member-down="${i}" ${i === se.cases.length - 1 ? 'disabled' : ''} title="${escapeAttr(t('case_step_down'))}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="trash" data-member-remove="${i}" title="${escapeAttr(t('suite_remove_case'))}"></tf-button>
      ` : ''}
    </div>
  `).join('');
  const swap = (i, j) => {
    if (j < 0 || j >= se.cases.length) return;
    [se.cases[i], se.cases[j]] = [se.cases[j], se.cases[i]];
    renderSuiteMembers(editable);
  };
  host.querySelectorAll('[data-member-up]').forEach((btn) => {
    btn.addEventListener('click', () => swap(Number(btn.dataset.memberUp), Number(btn.dataset.memberUp) - 1));
  });
  host.querySelectorAll('[data-member-down]').forEach((btn) => {
    btn.addEventListener('click', () => swap(Number(btn.dataset.memberDown), Number(btn.dataset.memberDown) + 1));
  });
  host.querySelectorAll('[data-member-remove]').forEach((btn) => {
    btn.addEventListener('click', () => {
      se.cases.splice(Number(btn.dataset.memberRemove), 1);
      renderSuitePool(editable);
      renderSuiteMembers(editable);
    });
  });
}

// =============================================================================
// T07/T08 — test runs (list + creation window)
// =============================================================================

async function loadRunsPage() {
  const s = f2();
  const resp = await ApiBinary.one('projectStudioRunsListRequest', {
    projectId: projectId(),
    status: s.runs.status,
    offset: (s.runs.page - 1) * F2_PAGE_SIZE,
    limit: F2_PAGE_SIZE,
  });
  s.runs.rows = Array.isArray(resp.runs) ? resp.runs : [];
  s.runs.total = Number(resp.total ?? s.runs.rows.length);
}

async function renderRunsView() {
  const host = byId('ps-tests-host');
  if (!host) return;
  const s = f2();
  try {
    await loadRunsPage();
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('runs_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || s.view !== 'runs') return;

  host.innerHTML = `
    <div class="ps-tests-toolbar">
      <tf-select id="ps-runs-f-status" value="${escapeAttr(s.runs.status)}">
        <option value="" ${s.runs.status === '' ? 'selected' : ''}>${escapeHtml(t('runs_filter_all'))}</option>
        ${['running', 'completed', 'cancelled'].map((x) => `<option value="${x}" ${x === s.runs.status ? 'selected' : ''}>${escapeHtml(t(`run_status_${x}`))}</option>`).join('')}
      </tf-select>
      <span class="ps-toolbar-spacer"></span>
      ${canEdit() ? `<tf-button variant="primary" icon="plus" id="ps-runs-new">${escapeHtml(t('runs_new'))}</tf-button>` : ''}
    </div>
    <div id="ps-runs-table-host">
      ${s.runs.rows.length ? '' : `<tf-empty-state icon="play" title="${escapeAttr(t('runs_empty'))}"></tf-empty-state>`}
    </div>
  `;
  byId('ps-runs-f-status')?.addEventListener('change', (e) => {
    s.runs.status = e.detail?.value ?? e.target.value ?? '';
    s.runs.page = 1;
    renderRunsView();
  });
  byId('ps-runs-new')?.addEventListener('click', () => openRunWindow({}));
  if (!s.runs.rows.length) return;

  byId('ps-runs-table-host').innerHTML = `
    <tf-table id="ps-runs-table" page-size="${F2_PAGE_SIZE}" total="${s.runs.total}" page="${s.runs.page}">
      <tf-column key="no" label="#"></tf-column>
      <tf-column key="name" label="${escapeAttr(t('runs_col_name'))}"></tf-column>
      <tf-column key="suite" label="${escapeAttr(t('runs_col_suite'))}"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('runs_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="result" label="${escapeAttr(t('runs_col_result'))}"></tf-column>
      <tf-column key="assignMode" label="${escapeAttr(t('runs_col_assign'))}"></tf-column>
      <tf-column key="createdBy" label="${escapeAttr(t('runs_col_created_by'))}"></tf-column>
      <tf-column key="startedAt" label="${escapeAttr(t('runs_col_started'))}"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-runs-table');
  const assignRows = () => {
    table.rows = s.runs.rows.map((run) => {
      const done = Number(run.passed ?? 0) + Number(run.failed ?? 0) + Number(run.blocked ?? 0) + Number(run.skipped ?? 0);
      return {
        _id: fv(run, 'run_id'),
        _row: run,
        no: `#${fv(run, 'run_no')}`,
        name: run.name,
        suite: fv(run, 'suite_name') || t('runs_adhoc'),
        status: chipCell(RUN_STATUS_CHIP[run.status], t(`run_status_${run.status}`)),
        result: t('runs_result', { done, total: Number(run.total ?? 0), passed: Number(run.passed ?? 0), failed: Number(run.failed ?? 0) }),
        assignMode: t(`assign_${fv(run, 'assignment_mode') || 'pool'}`),
        createdBy: fv(run, 'created_by_name') || '',
        startedAt: formatTimestamp(fv(run, 'started_at')),
      };
    });
  };
  assignRows();
  table.rowActions = (row) => {
    const wrap = document.createElement('div');
    wrap.className = 'ps-file-actions';
    const open = document.createElement('tf-button');
    open.setAttribute('variant', 'ghost');
    open.setAttribute('size', 'sm');
    open.setAttribute('icon', 'external-link');
    open.setAttribute('title', t('runs_open'));
    open.addEventListener('click', (e) => { e.stopPropagation(); openRunDetail(row._id); });
    wrap.appendChild(open);
    if (canManage() && row._row.status !== 'running') {
      const del = document.createElement('tf-button');
      del.setAttribute('variant', 'ghost');
      del.setAttribute('size', 'sm');
      del.setAttribute('icon', 'trash');
      del.setAttribute('title', t('action_delete'));
      del.addEventListener('click', (e) => { e.stopPropagation(); deleteRun(row._row); });
      wrap.appendChild(del);
    }
    return wrap;
  };
  table.addEventListener('row-click', (e) => {
    const runId = e.detail?.row?._id;
    if (runId) openRunDetail(runId);
  });
  table.addEventListener('page-change', async (e) => {
    s.runs.page = Number(e.detail?.page ?? 1);
    try {
      await loadRunsPage();
    } catch (err) {
      toast(`${t('runs_failed')}: ${err.message}`, 'error');
      return;
    }
    table.setAttribute('page', String(s.runs.page));
    table.setAttribute('total', String(s.runs.total));
    assignRows();
  });
}

function deleteRun(run) {
  openDeleteWindow({
    title: t('run_delete_title'),
    targetName: `#${fv(run, 'run_no')} ${run.name}`,
    targetSub: t(`run_status_${run.status}`),
    targetIcon: 'play',
    warning: t('run_delete_warning'),
    confirmLabel: t('delete_forever'),
    onConfirm: async () => {
      await ApiBinary.one('projectStudioRunDeleteRequest', { projectId: projectId(), runId: fv(run, 'run_id') });
      toast(t('run_delete_ok'), 'success');
      if (f2().view === 'run-detail') f2().view = 'runs';
      await renderTestsView();
    },
  });
}

// T08 — new run window. `prefill` supports { suiteId, suiteName } (from the
// suites list) and { fromFailedRunId, fromFailedLabel } (from run results).
async function openRunWindow(prefill = {}) {
  const { body, foot, cleanup } = openWindow({
    title: t('run_win_title'),
    subtitle: t('run_win_sub'),
    icon: 'play',
    width: 720,
  });

  const rw = {
    name: '',
    source: prefill.fromFailedRunId ? 'failed' : 'suite',
    suiteId: prefill.suiteId || '',
    caseIds: new Set(),
    fromFailedRunId: prefill.fromFailedRunId || '',
    envNote: '',
    mode: 'pool',
    singleAssignee: '',
    // caseId -> userId for per_case mode.
    assignments: new Map(),
    // Cases of the chosen source (needed to build per_case rows).
    sourceCases: [],
    pool: [],
    suites: [],
    completedRuns: [],
    busy: false,
  };

  body.innerHTML = `
    <div class="ps-field" style="margin-bottom:12px;">
      <tf-input id="ps-run-name" label="${escapeAttr(t('run_name_label'))}" placeholder="${escapeAttr(t('run_name_placeholder'))}"></tf-input>
    </div>
    <div class="ps-field" style="margin-bottom:12px;">
      <span class="ps-field-label">${escapeHtml(t('run_source_label'))}</span>
      <tf-segmented id="ps-run-source" value="${escapeAttr(rw.source)}">
        <option value="suite">${escapeHtml(t('run_source_suite'))}</option>
        <option value="cases">${escapeHtml(t('run_source_cases'))}</option>
        <option value="failed">${escapeHtml(t('run_source_failed'))}</option>
      </tf-segmented>
      <div class="ps-field-hint">${escapeHtml(t('run_source_hint'))}</div>
    </div>
    <div data-run-source-form="suite" hidden>
      <tf-select id="ps-run-suite" label="${escapeAttr(t('run_suite_label'))}" value="${escapeAttr(rw.suiteId)}"></tf-select>
    </div>
    <div data-run-source-form="cases" hidden>
      <div class="ps-field">
        <span class="ps-field-label">${escapeHtml(t('run_cases_label'))}</span>
        <div class="ps-pane-list ps-run-case-pool" data-run-case-pool></div>
      </div>
    </div>
    <div data-run-source-form="failed" hidden>
      <tf-select id="ps-run-failed" label="${escapeAttr(t('run_failed_label'))}" value="${escapeAttr(rw.fromFailedRunId)}"></tf-select>
      <div class="ps-field-hint">${escapeHtml(t('run_failed_hint'))}</div>
    </div>
    <div class="ps-field" style="margin:12px 0;">
      <tf-input id="ps-run-env" label="${escapeAttr(t('run_env_label'))}" placeholder="${escapeAttr(t('run_env_placeholder'))}"></tf-input>
    </div>
    <div class="ps-field" style="margin-bottom:12px;">
      <span class="ps-field-label">${escapeHtml(t('run_assign_label'))}</span>
      <tf-segmented id="ps-run-mode" value="${escapeAttr(rw.mode)}">
        <option value="single">${escapeHtml(t('assign_single'))}</option>
        <option value="per_case">${escapeHtml(t('assign_per_case'))}</option>
        <option value="pool">${escapeHtml(t('assign_pool'))}</option>
      </tf-segmented>
      <div class="ps-field-hint" data-assign-hint>${escapeHtml(t('assign_pool_hint'))}</div>
    </div>
    <div data-run-assign-form="single" hidden>
      <tf-select id="ps-run-assignee" label="${escapeAttr(t('run_assignee_label'))}" value=""></tf-select>
    </div>
    <div data-run-assign-form="per_case" hidden>
      <div class="ps-pane-list" data-run-assignments></div>
    </div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="primary" icon="play" data-action="create">${escapeHtml(t('run_create'))}</tf-button>
    </div>
  `;

  const showError = (msg) => {
    const el = body.querySelector('[data-form-error]');
    if (el) { el.hidden = !msg; el.textContent = msg || ''; }
  };
  const assignHints = { single: t('assign_single_hint'), per_case: t('assign_per_case_hint'), pool: t('assign_pool_hint') };

  const syncSourceForms = () => {
    body.querySelectorAll('[data-run-source-form]').forEach((form) => {
      form.hidden = form.dataset.runSourceForm !== rw.source;
    });
    // per_case assignments need a concrete case list; a "from failed" run only
    // materializes its items server-side, so the mode falls back to pool.
    if (rw.source === 'failed' && rw.mode === 'per_case') {
      rw.mode = 'pool';
      body.querySelector('#ps-run-mode')?.setAttribute('value', 'pool');
    }
    syncAssignForms();
  };
  const syncAssignForms = () => {
    body.querySelectorAll('[data-run-assign-form]').forEach((form) => {
      form.hidden = form.dataset.runAssignForm !== rw.mode;
    });
    const hint = body.querySelector('[data-assign-hint]');
    if (hint) hint.textContent = assignHints[rw.mode] || '';
    if (rw.mode === 'per_case') renderAssignments();
  };

  const currentSourceCases = () => {
    if (rw.source === 'cases') return rw.pool.filter((c) => rw.caseIds.has(fv(c, 'case_id')));
    if (rw.source === 'suite') return rw.sourceCases;
    return [];
  };

  const renderAssignments = () => {
    const hostEl = body.querySelector('[data-run-assignments]');
    if (!hostEl) return;
    const cases = currentSourceCases();
    if (!cases.length) {
      hostEl.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('assign_no_cases'))}</div>`;
      return;
    }
    const testers = testerMembers();
    hostEl.innerHTML = cases.map((c) => {
      const caseId = fv(c, 'case_id');
      const chosen = rw.assignments.get(caseId) || '';
      return `
        <div class="ps-pane-row">
          <div class="ps-pane-row-main"><div class="ps-pane-row-title">${escapeHtml(c.title)}</div></div>
          <tf-select data-assign-case="${escapeAttr(caseId)}" value="${escapeAttr(chosen)}">
            <option value="" ${chosen === '' ? 'selected' : ''}>${escapeHtml(t('assign_choose'))}</option>
            ${testers.map((m) => `<option value="${escapeAttr(fv(m, 'user_id'))}" ${fv(m, 'user_id') === chosen ? 'selected' : ''}>${escapeHtml(fv(m, 'display_name') || '')}</option>`).join('')}
          </tf-select>
        </div>
      `;
    }).join('');
    hostEl.querySelectorAll('[data-assign-case]').forEach((sel) => {
      sel.addEventListener('change', (e) => {
        rw.assignments.set(sel.dataset.assignCase, e.detail?.value ?? sel.value ?? '');
      });
    });
  };

  const renderCasePool = () => {
    const hostEl = body.querySelector('[data-run-case-pool]');
    if (!hostEl) return;
    if (!rw.pool.length) {
      hostEl.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('run_cases_empty'))}</div>`;
      return;
    }
    hostEl.innerHTML = rw.pool.map((c) => {
      const caseId = fv(c, 'case_id');
      return `
        <div class="ps-pane-row">
          <tf-checkbox data-run-case="${escapeAttr(caseId)}" ${rw.caseIds.has(caseId) ? 'checked' : ''}></tf-checkbox>
          <div class="ps-pane-row-main">
            <div class="ps-pane-row-title">${escapeHtml(c.title)}</div>
            <tf-chip status="${PRIORITY_CHIP[c.priority] || 'info'}">${escapeHtml(t(`prio_${c.priority}`))}</tf-chip>
          </div>
        </div>
      `;
    }).join('');
    hostEl.querySelectorAll('[data-run-case]').forEach((cb) => {
      cb.addEventListener('change', () => {
        const id = cb.dataset.runCase;
        if (cb.checked) rw.caseIds.add(id);
        else rw.caseIds.delete(id);
        if (rw.mode === 'per_case') renderAssignments();
      });
    });
  };

  body.querySelector('#ps-run-source')?.addEventListener('change', (e) => {
    rw.source = e.detail?.value ?? rw.source;
    syncSourceForms();
  });
  body.querySelector('#ps-run-mode')?.addEventListener('change', (e) => {
    const mode = e.detail?.value ?? rw.mode;
    if (mode === 'per_case' && rw.source === 'failed') {
      toast(t('assign_per_case_unavailable'), 'error');
      body.querySelector('#ps-run-mode')?.setAttribute('value', rw.mode);
      return;
    }
    rw.mode = mode;
    syncAssignForms();
  });
  body.querySelector('#ps-run-suite')?.addEventListener('change', async (e) => {
    rw.suiteId = e.detail?.value ?? '';
    rw.sourceCases = [];
    if (rw.suiteId) {
      try {
        const resp = await ApiBinary.one('projectStudioSuiteGetRequest', { projectId: projectId(), suiteId: rw.suiteId });
        rw.sourceCases = (Array.isArray(resp.cases) ? resp.cases : [])
          .slice()
          .sort((a, b) => Number(a.position ?? 0) - Number(b.position ?? 0));
      } catch { /* per_case list stays empty until the fetch succeeds */ }
    }
    if (rw.mode === 'per_case') renderAssignments();
  });
  body.querySelector('#ps-run-failed')?.addEventListener('change', (e) => {
    rw.fromFailedRunId = e.detail?.value ?? '';
  });
  body.querySelector('#ps-run-assignee')?.addEventListener('change', (e) => {
    rw.singleAssignee = e.detail?.value ?? '';
  });

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || rw.busy) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    rw.name = String(body.querySelector('#ps-run-name')?.value ?? '').trim();
    rw.envNote = String(body.querySelector('#ps-run-env')?.value ?? '').trim();
    if (rw.name.length < 3) { showError(t('err_run_name')); return; }
    if (rw.source === 'suite' && !rw.suiteId) { showError(t('err_run_suite')); return; }
    if (rw.source === 'cases' && !rw.caseIds.size) { showError(t('err_run_cases')); return; }
    if (rw.source === 'failed' && !rw.fromFailedRunId) { showError(t('err_run_failed')); return; }
    if (rw.mode === 'single' && !rw.singleAssignee) { showError(t('err_run_assignee')); return; }
    let assignments = [];
    if (rw.mode === 'per_case') {
      const cases = currentSourceCases();
      assignments = cases.map((c) => ({ caseId: fv(c, 'case_id'), userId: rw.assignments.get(fv(c, 'case_id')) || '' }));
      if (!cases.length || assignments.some((a) => !a.userId)) { showError(t('err_run_assignments')); return; }
    }
    showError(null);
    rw.busy = true;
    btn.setAttribute('disabled', '');
    try {
      const resp = await ApiBinary.one('projectStudioRunCreateRequest', {
        projectId: projectId(),
        name: rw.name,
        suiteId: rw.source === 'suite' ? rw.suiteId : '',
        caseIds: rw.source === 'cases' ? [...rw.caseIds] : [],
        fromFailedRunId: rw.source === 'failed' ? rw.fromFailedRunId : '',
        envNote: rw.envNote,
        assignmentMode: rw.mode,
        singleAssignee: rw.mode === 'single' ? rw.singleAssignee : '',
        assignments,
      });
      toast(t('run_created', { no: Number(fv(resp, 'run_no') ?? 0) }), 'success');
      cleanup();
      const runId = fv(resp, 'run_id');
      if (runId) await openRunDetail(runId);
    } catch (err) {
      rw.busy = false;
      btn.removeAttribute('disabled');
      showError(`${t('run_create_failed')}: ${err.message}`);
    }
  });

  // Suites / approved cases / completed runs / members load lazily.
  (async () => {
    await ensureF2Members();
    const testers = testerMembers();
    // Connected tf-selects only accept new options via setOptions().
    body.querySelector('#ps-run-assignee')?.setOptions([
      { value: '', label: t('assign_choose') },
      ...testers.map((m) => ({ value: fv(m, 'user_id'), label: fv(m, 'display_name') || '' })),
    ], '');
    try {
      const resp = await ApiBinary.one('projectStudioSuitesListRequest', { projectId: projectId() });
      rw.suites = Array.isArray(resp.suites) ? resp.suites : [];
    } catch { rw.suites = []; }
    body.querySelector('#ps-run-suite')?.setOptions([
      { value: '', label: t('assign_choose') },
      ...rw.suites.map((su) => ({
        value: fv(su, 'suite_id'),
        label: `${su.name} (${Number(fv(su, 'case_count') ?? 0)})`,
      })),
    ], rw.suiteId || '');
    if (rw.suiteId) {
      try {
        const resp = await ApiBinary.one('projectStudioSuiteGetRequest', { projectId: projectId(), suiteId: rw.suiteId });
        rw.sourceCases = Array.isArray(resp.cases) ? resp.cases : [];
      } catch { /* per_case list stays empty */ }
    }
    try {
      const resp = await ApiBinary.one('projectStudioCasesListRequest', {
        projectId: projectId(), status: 'approved', offset: 0, limit: 200,
      });
      rw.pool = Array.isArray(resp.cases) ? resp.cases : [];
    } catch { rw.pool = []; }
    renderCasePool();
    try {
      const resp = await ApiBinary.one('projectStudioRunsListRequest', {
        projectId: projectId(), status: 'completed', offset: 0, limit: 50,
      });
      rw.completedRuns = (Array.isArray(resp.runs) ? resp.runs : [])
        .filter((r) => Number(r.failed ?? 0) + Number(r.blocked ?? 0) > 0);
    } catch { rw.completedRuns = []; }
    body.querySelector('#ps-run-failed')?.setOptions([
      { value: '', label: t('assign_choose') },
      ...rw.completedRuns.map((r) => ({
        value: fv(r, 'run_id'),
        label: `#${fv(r, 'run_no')} ${r.name} (${Number(r.failed ?? 0)}+${Number(r.blocked ?? 0)})`,
      })),
    ], rw.fromFailedRunId || '');
    syncSourceForms();
  })();
}

// =============================================================================
// T11 — run detail (results, expandable items, my items, CSV export)
// =============================================================================

async function openRunDetail(runId) {
  stopTestsLive();
  f2().runDetail = { runId, run: null, items: [] };
  f2().view = 'run-detail';
  await renderTestsView();
}

async function renderRunDetailView() {
  const host = byId('ps-tests-host');
  const s = f2();
  const rd = s.runDetail;
  if (!host || !rd) { s.view = 'runs'; return renderRunsView(); }
  try {
    const resp = await ApiBinary.one('projectStudioRunGetRequest', { projectId: projectId(), runId: rd.runId });
    rd.run = resp.run || null;
    rd.items = Array.isArray(resp.items) ? resp.items : [];
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('run_load_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || s.view !== 'run-detail') return;
  const run = rd.run;
  if (!run) { s.view = 'runs'; return renderRunsView(); }
  const running = run.status === 'running';
  const creator = isMe(fv(run, 'created_by'));
  const canClose = running && (canManage() || creator);
  const failedCount = Number(run.failed ?? 0) + Number(run.blocked ?? 0);
  const myItems = rd.items.filter((it) => {
    const assignee = fv(it, 'assigned_to');
    if (isMe(assignee)) return it.status === 'pending' || it.status === 'in_progress';
    return assignee === '' && it.status === 'pending';
  });

  host.innerHTML = `
    <div class="ps-editor-head">
      <tf-button variant="ghost" icon="chevron-left" id="ps-run-back">${escapeHtml(t('back_to_runs'))}</tf-button>
      <div class="ps-editor-title-static">
        <div class="ps-detail-name">#${fv(run, 'run_no')} ${escapeHtml(run.name)}</div>
        <div class="ps-detail-sub">
          ${escapeHtml(fv(run, 'suite_name') || t('runs_adhoc'))} ·
          ${escapeHtml(t(`assign_${fv(run, 'assignment_mode') || 'pool'}`))} ·
          ${escapeHtml(fv(run, 'created_by_name') || '')} · ${escapeHtml(formatTimestamp(fv(run, 'started_at')))}
        </div>
      </div>
      <div class="ps-editor-badges">
        <tf-chip status="${RUN_STATUS_CHIP[run.status] || 'info'}" dot>${escapeHtml(t(`run_status_${run.status}`))}</tf-chip>
        ${fv(run, 'env_note') ? `<tf-chip status="info">${escapeHtml(fv(run, 'env_note'))}</tf-chip>` : ''}
      </div>
      <div class="ps-editor-actions">
        <tf-button variant="ghost" icon="download" id="ps-run-export">${escapeHtml(t('run_export_csv'))}</tf-button>
        ${canEdit() && failedCount > 0 && !running ? `<tf-button variant="ghost" icon="rotate" id="ps-run-from-failed">${escapeHtml(t('run_new_from_failed'))}</tf-button>` : ''}
        ${canClose ? `<tf-button variant="ghost" icon="check" id="ps-run-close">${escapeHtml(t('run_close'))}</tf-button>` : ''}
        ${canClose ? `<tf-button variant="ghost" icon="ban" id="ps-run-cancel">${escapeHtml(t('run_cancel'))}</tf-button>` : ''}
        ${canManage() && !running ? `<tf-button variant="ghost" icon="trash" id="ps-run-delete" title="${escapeAttr(t('action_delete'))}"></tf-button>` : ''}
      </div>
    </div>

    <div class="ps-kpi-grid ps-run-kpis">
      <tf-stat-card size="sm" icon="check" accent="success" label="${escapeAttr(t('item_status_passed'))}" value="${Number(run.passed ?? 0)}"></tf-stat-card>
      <tf-stat-card size="sm" icon="x" accent="danger" label="${escapeAttr(t('item_status_failed'))}" value="${Number(run.failed ?? 0)}"></tf-stat-card>
      <tf-stat-card size="sm" icon="ban" accent="warning" label="${escapeAttr(t('item_status_blocked'))}" value="${Number(run.blocked ?? 0)}"></tf-stat-card>
      <tf-stat-card size="sm" icon="chevron-right" label="${escapeAttr(t('item_status_skipped'))}" value="${Number(run.skipped ?? 0)}"></tf-stat-card>
      <tf-stat-card size="sm" icon="clock" label="${escapeAttr(t('run_kpi_open'))}" value="${Number(run.pending ?? 0) + Number(run.in_progress ?? fv(run, 'in_progress') ?? 0)}"></tf-stat-card>
    </div>
    <div class="ps-run-progressbar" id="ps-run-stacked"></div>

    ${canTest() && running ? `
      <tf-section-card title="${escapeAttr(t('run_my_items_title'))}" icon="user">
        <span slot="subtitle">${escapeHtml(t('run_my_items_sub', { count: myItems.length }))}</span>
        <span slot="actions">
          <tf-button variant="primary" size="sm" icon="play" id="ps-run-claim-next">${escapeHtml(t('run_claim_next'))}</tf-button>
        </span>
        <div id="ps-run-my-items">
          ${myItems.length ? myItems.map((it) => `
            <div class="ps-pane-row">
              <tf-chip status="${ITEM_STATUS_CHIP[it.status] || 'info'}" dot>${escapeHtml(t(`item_status_${it.status}`))}</tf-chip>
              <div class="ps-pane-row-main">
                <div class="ps-pane-row-title">${escapeHtml(fv(it, 'case_title'))}</div>
                <span class="ps-pane-row-sub">${escapeHtml(fv(it, 'assigned_to') ? t('run_item_assigned_you') : t('run_item_pool'))}</span>
              </div>
              <tf-button variant="ghost" size="sm" icon="play" data-claim-item="${escapeAttr(fv(it, 'item_id'))}">${escapeHtml(it.status === 'in_progress' ? t('run_item_continue') : t('run_item_execute'))}</tf-button>
            </div>
          `).join('') : `<div class="ps-field-hint">${escapeHtml(t('run_my_items_empty'))}</div>`}
        </div>
      </tf-section-card>
    ` : ''}

    <tf-section-card title="${escapeAttr(t('run_items_title'))}" icon="list">
      <span slot="subtitle">${escapeHtml(t('run_items_sub', { count: rd.items.length }))}</span>
      <div id="ps-run-items-host"></div>
    </tf-section-card>
  `;

  const stacked = byId('ps-run-stacked');
  if (stacked && Number(run.total ?? 0) > 0) {
    const bar = document.createElement('tf-bar-chart');
    bar.mode = 'single';
    bar.height = 14;
    bar.showLegend = true;
    bar.total = Number(run.total ?? 0);
    bar.segments = [
      { id: 'passed', label: t('item_status_passed'), value: Number(run.passed ?? 0), tone: 'success' },
      { id: 'failed', label: t('item_status_failed'), value: Number(run.failed ?? 0), tone: 'critical' },
      { id: 'blocked', label: t('item_status_blocked'), value: Number(run.blocked ?? 0), tone: 'warning' },
      { id: 'skipped', label: t('item_status_skipped'), value: Number(run.skipped ?? 0), tone: 'muted' },
      { id: 'in_progress', label: t('item_status_in_progress'), value: Number(run.in_progress ?? fv(run, 'in_progress') ?? 0), tone: 'info' },
    ];
    stacked.replaceChildren(bar);
  }

  byId('ps-run-back')?.addEventListener('click', () => { s.runDetail = null; s.view = 'runs'; renderTestsView(); });
  byId('ps-run-export')?.addEventListener('click', () => exportRunCsv(rd));
  byId('ps-run-from-failed')?.addEventListener('click', () => openRunWindow({ fromFailedRunId: rd.runId }));
  byId('ps-run-close')?.addEventListener('click', () => closeRun(rd.runId, false));
  byId('ps-run-cancel')?.addEventListener('click', () => closeRun(rd.runId, true));
  byId('ps-run-delete')?.addEventListener('click', () => deleteRun(run));
  byId('ps-run-claim-next')?.addEventListener('click', () => claimAndExec(rd.runId, null));
  byId('ps-run-my-items')?.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-claim-item]');
    if (!btn) return;
    const item = rd.items.find((it) => fv(it, 'item_id') === btn.dataset.claimItem);
    if (item?.status === 'in_progress' && isMe(fv(item, 'assigned_to'))) {
      openExecItem(btn.dataset.claimItem);
    } else {
      claimAndExec(rd.runId, btn.dataset.claimItem);
    }
  });

  renderRunItemsTable(rd);
}

function renderRunItemsTable(rd) {
  const hostEl = byId('ps-run-items-host');
  if (!hostEl) return;
  if (!rd.items.length) {
    hostEl.innerHTML = `<tf-empty-state icon="list" title="${escapeAttr(t('run_items_empty'))}"></tf-empty-state>`;
    return;
  }
  hostEl.innerHTML = `
    <tf-table id="ps-run-items-table">
      <tf-column key="pos" label="#" renderer="num"></tf-column>
      <tf-column key="title" label="${escapeAttr(t('run_items_col_case'))}"></tf-column>
      <tf-column key="assignee" label="${escapeAttr(t('run_items_col_assignee'))}"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('run_items_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="steps" label="${escapeAttr(t('run_items_col_steps'))}"></tf-column>
      <tf-column key="duration" label="${escapeAttr(t('run_items_col_duration'))}"></tf-column>
      <tf-column key="finished" label="${escapeAttr(t('run_items_col_finished'))}"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-run-items-table');
  table.rowKey = '_id';
  table.expandable = true;
  table.expandRenderer = (row) => buildRunItemExpansion(row._row);
  table.rows = rd.items.map((it) => ({
    _id: fv(it, 'item_id'),
    _row: it,
    pos: Number(it.position ?? 0) + 1,
    title: fv(it, 'case_title'),
    assignee: fv(it, 'assigned_to_name') || t('run_item_pool'),
    status: chipCell(ITEM_STATUS_CHIP[it.status], t(`item_status_${it.status}`)),
    steps: `${Number(fv(it, 'steps_done') ?? 0)}/${Number(fv(it, 'steps_total') ?? 0)}`,
    duration: formatDuration(Number(fv(it, 'duration_secs') ?? 0)),
    finished: fv(it, 'finished_at') ? formatTimestamp(fv(it, 'finished_at')) : '—',
  }));
}

// Expansion region: step verdicts + notes + attachment thumbnails, loaded
// lazily per expanded row (the run payload carries items only).
function buildRunItemExpansion(item) {
  const wrap = document.createElement('div');
  wrap.className = 'ps-item-expansion';
  wrap.innerHTML = `<div class="ps-loading">${escapeHtml(t('loading'))}</div>`;
  (async () => {
    let resp = null;
    try {
      resp = await ApiBinary.one('projectStudioRunItemGetRequest', { projectId: projectId(), itemId: fv(item, 'item_id') });
    } catch (err) {
      wrap.innerHTML = `<div class="ps-form-error">${escapeHtml(err.message)}</div>`;
      return;
    }
    if (!wrap.isConnected) return;
    const steps = Array.isArray(resp.steps) ? resp.steps : [];
    const itemAtts = Array.isArray(resp.item?.attachments) ? resp.item.attachments : [];
    wrap.innerHTML = `
      ${fv(resp.item || {}, 'result_note') ? `<div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(fv(resp.item, 'result_note'))}</span></div>` : ''}
      ${steps.map((st) => `
        <div class="ps-exp-step">
          <div class="ps-step-num">${Number(fv(st, 'step_index') ?? 0) + 1}</div>
          <div class="ps-exp-step-main">
            <div class="ps-exp-step-action">${escapeHtml(st.action)}</div>
            <div class="ps-exp-step-expected">${escapeHtml(st.expected)}</div>
            ${st.note ? `<div class="ps-exp-step-note">${escapeHtml(st.note)}</div>` : ''}
          </div>
          <tf-chip status="${ITEM_STATUS_CHIP[st.status] || 'info'}" dot>${escapeHtml(st.status ? t(`item_status_${st.status}`) : t('item_status_pending'))}</tf-chip>
        </div>
      `).join('')}
      <div class="ps-exp-atts" data-exp-atts></div>
    `;
    const allAtts = [
      ...itemAtts,
      ...steps.flatMap((st) => (Array.isArray(st.attachments) ? st.attachments : [])),
    ];
    const attHost = wrap.querySelector('[data-exp-atts]');
    if (attHost && allAtts.length) {
      for (const att of allAtts) {
        const mime = fv(att, 'mime') || '';
        const cell = document.createElement('div');
        cell.className = 'ps-exp-att';
        cell.title = fv(att, 'name') || '';
        if (mime.startsWith('image/')) {
          const img = document.createElement('img');
          img.alt = fv(att, 'name') || '';
          cell.appendChild(img);
          fetchAttachmentBlob(att).then(({ blob }) => {
            if (img.isConnected || cell.isConnected) img.src = URL.createObjectURL(blob);
          }).catch(() => { cell.textContent = fv(att, 'name') || ''; });
        } else {
          cell.textContent = fv(att, 'name') || '';
        }
        cell.addEventListener('click', () => openAttachmentPreview(att));
        attHost.appendChild(cell);
      }
    }
  })();
  return wrap;
}

function formatDuration(secs) {
  const n = Number(secs) || 0;
  if (n <= 0) return '—';
  const m = Math.floor(n / 60);
  const sRest = n % 60;
  return m > 0 ? `${m}m ${sRest}s` : `${sRest}s`;
}

async function closeRun(runId, cancelled) {
  const ok = await TfWindow.confirm({
    title: t(cancelled ? 'run_cancel_title' : 'run_close_title'),
    message: t(cancelled ? 'run_cancel_message' : 'run_close_message'),
    confirmLabel: t(cancelled ? 'run_cancel' : 'run_close'),
    cancelLabel: t('action_cancel'),
    danger: cancelled,
  });
  if (!ok) return;
  try {
    await ApiBinary.one('projectStudioRunCloseRequest', { projectId: projectId(), runId, cancelled });
    toast(t(cancelled ? 'run_cancelled_ok' : 'run_closed_ok'), 'success');
    await renderTestsView();
  } catch (err) {
    toast(`${t('run_close_failed')}: ${err.message}`, 'error');
  }
}

function exportRunCsv(rd) {
  const rows = rd.items.map((it) => ({
    position: Number(it.position ?? 0) + 1,
    case_title: fv(it, 'case_title'),
    case_version: fv(it, 'case_version'),
    assigned_to: fv(it, 'assigned_to_name') || '',
    status: it.status,
    steps_done: fv(it, 'steps_done'),
    steps_total: fv(it, 'steps_total'),
    duration_secs: fv(it, 'duration_secs'),
    result_note: fv(it, 'result_note') || '',
    finished_at: fv(it, 'finished_at') || '',
  }));
  downloadTextFile(`run-${fv(rd.run, 'run_no')}.csv`, toCsv(rows));
}

// =============================================================================
// T09 — tester execution desk
// =============================================================================

async function claimAndExec(runId, itemId) {
  try {
    const resp = await ApiBinary.one('projectStudioRunItemClaimRequest', { projectId: projectId(), runId, itemId });
    const item = resp.item;
    if (!item) {
      toast(t('exec_nothing_left'), 'info');
      return;
    }
    await openExecItem(fv(item, 'item_id'));
  } catch (err) {
    toast(`${t('exec_claim_failed')}: ${err.message}`, 'error');
  }
}

async function openExecItem(itemId) {
  stopTestsLive();
  f2().exec = { itemId, item: null, steps: [], preconditions: '', testData: '', startedMs: Date.now(), timerId: null, resultNote: '', testerConfig: '', override: '', attachments: [] };
  f2().view = 'exec';
  await renderTestsView();
}

async function renderExecView() {
  const host = byId('ps-tests-host');
  const s = f2();
  const ex = s.exec;
  if (!host || !ex) { s.view = 'runs'; return renderRunsView(); }
  try {
    const resp = await ApiBinary.one('projectStudioRunItemGetRequest', { projectId: projectId(), itemId: ex.itemId });
    ex.item = resp.item || null;
    ex.steps = (Array.isArray(resp.steps) ? resp.steps : []).map((st) => ({
      index: Number(fv(st, 'step_index') ?? 0),
      action: st.action,
      expected: st.expected,
      status: st.status || '',
      note: st.note || '',
      attachments: Array.isArray(st.attachments) ? st.attachments.slice() : [],
      pendingStatus: '',
    }));
    ex.preconditions = resp.preconditions || '';
    ex.testData = fv(resp, 'test_data') || '';
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('exec_load_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || s.view !== 'exec') return;
  const item = ex.item;
  if (!item) { s.view = 'runs'; return renderRunsView(); }
  const claimedAt = fv(item, 'claimed_at');
  if (claimedAt) {
    const parsed = new Date(String(claimedAt).includes('T') ? claimedAt : `${String(claimedAt).replace(' ', 'T')}Z`);
    if (!Number.isNaN(parsed.getTime())) ex.startedMs = parsed.getTime();
  }
  ex.resultNote = fv(item, 'result_note') || '';
  ex.testerConfig = fv(item, 'tester_config') || '';
  ex.attachments = Array.isArray(item.attachments) ? item.attachments.slice() : [];

  host.innerHTML = `
    <div class="ps-editor-head">
      <tf-button variant="ghost" icon="chevron-left" id="ps-exec-back">${escapeHtml(t('exec_back'))}</tf-button>
      <div class="ps-editor-title-static">
        <div class="ps-detail-name">${escapeHtml(fv(item, 'case_title'))}</div>
        <div class="ps-detail-sub">${escapeHtml(t('exec_sub', { version: Number(fv(item, 'case_version') ?? 1) }))}</div>
      </div>
      <div class="ps-editor-badges">
        <tf-chip status="${ITEM_STATUS_CHIP[item.status] || 'info'}" dot>${escapeHtml(t(`item_status_${item.status}`))}</tf-chip>
        <tf-chip status="info">${sprite('clock')} <span id="ps-exec-clock">0:00</span></tf-chip>
      </div>
      <div class="ps-editor-actions">
        <tf-button variant="ghost" icon="alert" id="ps-exec-defect">${escapeHtml(t('exec_report_defect'))}</tf-button>
        <tf-button variant="ghost" icon="rotate" id="ps-exec-release">${escapeHtml(t('exec_release'))}</tf-button>
        <tf-button variant="primary" icon="check" id="ps-exec-finish">${escapeHtml(t('exec_finish'))}</tf-button>
      </div>
    </div>
    <div class="ps-exec-progress">
      <tf-progress-bar id="ps-exec-progress" value="0"></tf-progress-bar>
      <span id="ps-exec-progress-label"></span>
    </div>
    ${ex.preconditions ? `
      <tf-section-card title="${escapeAttr(t('case_preconditions_title'))}" icon="info">
        <div class="ps-version-block">${escapeHtml(ex.preconditions)}</div>
      </tf-section-card>
    ` : ''}
    ${ex.testData ? `
      <tf-section-card title="${escapeAttr(t('case_test_data_title'))}" icon="database">
        <div class="ps-version-block">${escapeHtml(ex.testData)}</div>
      </tf-section-card>
    ` : ''}
    <tf-section-card title="${escapeAttr(t('case_steps_title'))}" icon="list">
      <span slot="subtitle">${escapeHtml(t('exec_steps_sub'))}</span>
      <div id="ps-exec-steps"></div>
    </tf-section-card>
    <tf-section-card title="${escapeAttr(t('exec_summary_title'))}" icon="check">
      <div class="ps-exec-summary-grid">
        <tf-input id="ps-exec-config" label="${escapeAttr(t('exec_config_label'))}" placeholder="${escapeAttr(t('exec_config_placeholder'))}" value="${escapeAttr(ex.testerConfig)}"></tf-input>
        <tf-select id="ps-exec-override" label="${escapeAttr(t('exec_override_label'))}" value="">
          <option value="" selected>${escapeHtml(t('exec_override_auto'))}</option>
          ${['passed', 'failed', 'blocked', 'skipped'].map((x) => `<option value="${x}">${escapeHtml(t(`item_status_${x}`))}</option>`).join('')}
        </tf-select>
      </div>
      <tf-textarea id="ps-exec-note" rows="2" label="${escapeAttr(t('exec_note_label'))}" hint="${escapeAttr(t('exec_note_hint'))}"></tf-textarea>
      <div class="ps-field">
        <span class="ps-field-label">${escapeHtml(t('case_attachments_title'))}</span>
        <div id="ps-exec-atts"></div>
        <tf-file-input id="ps-exec-att-input" multiple label="${escapeAttr(t('attachment_dropzone'))}"></tf-file-input>
      </div>
    </tf-section-card>
  `;

  const noteEl = byId('ps-exec-note');
  if (noteEl) {
    noteEl.value = ex.resultNote;
    noteEl.addEventListener('input', () => { ex.resultNote = String(noteEl.value ?? ''); });
  }
  byId('ps-exec-config')?.addEventListener('input', (e) => { ex.testerConfig = String(e.target.value ?? ''); });
  byId('ps-exec-override')?.addEventListener('change', (e) => { ex.override = e.detail?.value ?? ''; });
  byId('ps-exec-back')?.addEventListener('click', () => backToRunFromExec());
  byId('ps-exec-release')?.addEventListener('click', () => releaseExecItem());
  byId('ps-exec-finish')?.addEventListener('click', () => finishExecItem());
  byId('ps-exec-defect')?.addEventListener('click', () => {
    const run = s.runDetail?.run;
    openTaskWindow({
      taskType: 'defect',
      links: [
        { kind: 'run', id: fv(item, 'run_id'), label: run ? `#${fv(run, 'run_no')} ${run.name}` : t('exec_link_run') },
        { kind: 'run_item', id: fv(item, 'item_id'), label: fv(item, 'case_title') },
      ],
      titlePrefill: t('exec_defect_title_prefill', { title: fv(item, 'case_title') }),
    });
  });
  byId('ps-exec-att-input')?.addEventListener('change', async (e) => {
    const files = e.detail?.files;
    if (!files || !files.length) return;
    try {
      for (const file of Array.from(files)) {
        const att = await uploadAttachmentFile(file);
        if (!ex.attachments.some((a) => fv(a, 'sha256') === att.sha256)) ex.attachments.push(att);
      }
      renderExecItemAtts();
      toast(t('attachment_uploaded'), 'success');
    } catch (err) {
      toast(`${t('attachment_upload_failed')}: ${err.message}`, 'error');
    }
  });

  renderExecSteps();
  renderExecItemAtts();
  updateExecProgress();

  // Live duration ticker (claimed_at is the anchor when present).
  const clock = byId('ps-exec-clock');
  const tick = () => {
    if (!clock || !clock.isConnected) return;
    const secs = Math.max(0, Math.floor((Date.now() - ex.startedMs) / 1000));
    clock.textContent = `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, '0')}`;
  };
  tick();
  ex.timerId = setInterval(tick, 1000);
}

function renderExecSteps() {
  const ex = f2().exec;
  const host = byId('ps-exec-steps');
  if (!host || !ex) return;
  const verdicts = [
    { id: 'passed', icon: 'check', label: t('item_status_passed') },
    { id: 'failed', icon: 'x', label: t('item_status_failed') },
    { id: 'blocked', icon: 'ban', label: t('item_status_blocked') },
    { id: 'skipped', icon: 'chevron-right', label: t('item_status_skipped') },
  ];
  host.innerHTML = ex.steps.map((step, i) => {
    const effective = step.pendingStatus || step.status;
    return `
      <div class="ps-exec-step ${effective ? `is-${effective}` : ''}" data-exec-step="${i}">
        <div class="ps-step-num">${step.index + 1}</div>
        <div class="ps-exec-step-main">
          <div class="ps-exp-step-action">${escapeHtml(step.action)}</div>
          <div class="ps-exp-step-expected">${escapeHtml(step.expected)}</div>
          <div class="ps-verdict-row">
            ${verdicts.map((v) => `
              <tf-button size="sm" variant="${effective === v.id ? (v.id === 'passed' ? 'primary' : 'danger-solid') : 'ghost'}"
                icon="${v.icon}" data-verdict="${v.id}" data-verdict-step="${i}">${escapeHtml(v.label)}</tf-button>
            `).join('')}
            ${step.pendingStatus ? `<tf-chip status="warn">${escapeHtml(t('exec_step_note_required'))}</tf-chip>` : ''}
          </div>
          <div class="ps-exec-step-note">
            <tf-textarea rows="1" data-step-note="${i}" placeholder="${escapeAttr(t('exec_step_note_placeholder'))}"></tf-textarea>
            <tf-file-input data-step-att="${i}" accept="image/*" label="${escapeAttr(t('exec_step_attach'))}"></tf-file-input>
          </div>
          ${step.attachments.length ? `
            <div class="ps-exp-atts">
              ${step.attachments.map((a, ai) => `<tf-chip status="info" data-step-att-open="${i}:${ai}" role="button" tabindex="0">${sprite('paperclip')} ${escapeHtml(fv(a, 'name') || '')}</tf-chip>`).join('')}
            </div>
          ` : ''}
        </div>
      </div>
    `;
  }).join('');

  ex.steps.forEach((step, i) => {
    const noteEl = host.querySelector(`[data-step-note="${i}"]`);
    if (noteEl) {
      noteEl.value = step.note;
      noteEl.addEventListener('input', () => { step.note = String(noteEl.value ?? ''); });
      noteEl.addEventListener('change', () => {
        // A pending fail/blocked verdict waits for the note — send it as soon
        // as the tester provides one (verdicts persist immediately by design).
        if (step.pendingStatus && step.note.trim()) sendExecStep(i, step.pendingStatus);
      });
    }
    const attEl = host.querySelector(`[data-step-att="${i}"]`);
    attEl?.addEventListener('change', async (e) => {
      const files = e.detail?.files;
      if (!files || !files.length) return;
      try {
        for (const file of Array.from(files)) {
          const att = await uploadAttachmentFile(file);
          if (!step.attachments.some((a) => fv(a, 'sha256') === att.sha256)) step.attachments.push(att);
        }
        if (step.status) await sendExecStep(i, step.status);
        renderExecSteps();
        toast(t('attachment_uploaded'), 'success');
      } catch (err) {
        toast(`${t('attachment_upload_failed')}: ${err.message}`, 'error');
      }
    });
  });
  host.querySelectorAll('[data-verdict]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const i = Number(btn.dataset.verdictStep);
      const verdict = btn.dataset.verdict;
      const step = ex.steps[i];
      if (!step) return;
      if ((verdict === 'failed' || verdict === 'blocked') && !step.note.trim()) {
        step.pendingStatus = verdict;
        renderExecSteps();
        toast(t('exec_note_required_toast'), 'error');
        host.querySelector(`[data-step-note="${i}"]`)?.focus?.();
        return;
      }
      sendExecStep(i, verdict);
    });
  });
  host.querySelectorAll('[data-step-att-open]').forEach((chipEl) => {
    chipEl.addEventListener('click', () => {
      const [i, ai] = chipEl.dataset.stepAttOpen.split(':').map(Number);
      const att = ex.steps[i]?.attachments[ai];
      if (att) openAttachmentPreview(att);
    });
  });
}

// Persists a single step verdict IMMEDIATELY (T09 contract: every verdict is
// durable the moment it is chosen, a browser crash loses nothing).
async function sendExecStep(i, status) {
  const ex = f2().exec;
  const step = ex?.steps[i];
  if (!step) return;
  try {
    await ApiBinary.one('projectStudioRunStepSetRequest', {
      projectId: projectId(),
      itemId: ex.itemId,
      stepIndex: step.index,
      status,
      note: step.note,
      attachmentsJson: JSON.stringify(step.attachments),
    });
    step.status = status;
    step.pendingStatus = '';
    renderExecSteps();
    updateExecProgress();
  } catch (err) {
    toast(`${t('exec_step_failed')}: ${err.message}`, 'error');
  }
}

function updateExecProgress() {
  const ex = f2().exec;
  if (!ex) return;
  const done = ex.steps.filter((s) => !!s.status).length;
  const total = Math.max(1, ex.steps.length);
  byId('ps-exec-progress')?.setAttribute('value', String(Math.round((done / total) * 100)));
  const label = byId('ps-exec-progress-label');
  if (label) label.textContent = t('exec_progress_label', { done, total: ex.steps.length });
}

function renderExecItemAtts() {
  const ex = f2().exec;
  const host = byId('ps-exec-atts');
  if (!host || !ex) return;
  host.innerHTML = ex.attachments.length
    ? ex.attachments.map((att, i) => attachmentRowHtml(att, i, { removable: true })).join('')
    : `<div class="ps-field-hint">${escapeHtml(t('attachments_empty'))}</div>`;
  host.querySelectorAll('[data-att-preview]').forEach((btn) => {
    btn.addEventListener('click', () => openAttachmentPreview(ex.attachments[Number(btn.dataset.attPreview)]));
  });
  host.querySelectorAll('[data-att-remove]').forEach((btn) => {
    btn.addEventListener('click', () => {
      ex.attachments.splice(Number(btn.dataset.attRemove), 1);
      renderExecItemAtts();
    });
  });
}

function backToRunFromExec() {
  const s = f2();
  stopTestsLive();
  const runId = s.exec?.item ? fv(s.exec.item, 'run_id') : s.runDetail?.runId;
  s.exec = null;
  if (runId) openRunDetail(runId);
  else { s.view = 'runs'; renderTestsView(); }
}

async function releaseExecItem() {
  const ex = f2().exec;
  if (!ex) return;
  try {
    await ApiBinary.one('projectStudioRunItemReleaseRequest', { projectId: projectId(), itemId: ex.itemId });
    toast(t('exec_released'), 'success');
    backToRunFromExec();
  } catch (err) {
    toast(`${t('exec_release_failed')}: ${err.message}`, 'error');
  }
}

async function finishExecItem() {
  const ex = f2().exec;
  if (!ex) return;
  if (ex.override && !ex.resultNote.trim()) {
    toast(t('exec_override_note_required'), 'error');
    return;
  }
  const durationSecs = Math.max(1, Math.floor((Date.now() - ex.startedMs) / 1000));
  let resp = null;
  try {
    resp = await ApiBinary.one('projectStudioRunItemFinishRequest', {
      projectId: projectId(),
      itemId: ex.itemId,
      status: ex.override || '',
      resultNote: ex.resultNote,
      testerConfig: ex.testerConfig,
      durationSecs,
      attachmentsJson: JSON.stringify(ex.attachments),
    });
  } catch (err) {
    toast(`${t('exec_finish_failed')}: ${err.message}`, 'error');
    return;
  }
  toast(t('exec_finished'), 'success');
  const next = fv(resp, 'next_item');
  if (next) {
    const goNext = await TfWindow.confirm({
      title: t('exec_next_title'),
      message: t('exec_next_message', { title: fv(next, 'case_title') }),
      confirmLabel: t('exec_next_go'),
      cancelLabel: t('exec_next_back'),
    });
    if (goNext) {
      await claimAndExec(fv(next, 'run_id'), fv(next, 'item_id'));
      return;
    }
  }
  backToRunFromExec();
}

// =============================================================================
// T14 — reports (5 report kinds over the generic ReportQuery; CSV client-side)
// =============================================================================

const REPORT_KINDS = ['runs_over_time', 'suite_pass_rate', 'tester_stats', 'source_coverage', 'defects'];

// rows_json schema is per report and owned by the backend; the UI reads the
// first matching key from a candidate list so field naming stays flexible.
function rowVal(row, keys, dflt = null) {
  for (const k of keys) {
    if (row && row[k] != null) return row[k];
  }
  return dflt;
}

async function renderReportsView() {
  const host = byId('ps-tests-host');
  if (!host) return;
  const s = f2();
  const rep = s.reports;
  if (!s.suites.length) {
    try {
      const resp = await ApiBinary.one('projectStudioSuitesListRequest', { projectId: projectId() });
      s.suites = Array.isArray(resp.suites) ? resp.suites : [];
    } catch { /* suite filter stays empty */ }
  }
  if (state.tab !== 'tests' || s.view !== 'reports') return;

  host.innerHTML = `
    <div class="ps-tests-toolbar ps-reports-toolbar">
      <tf-input id="ps-rep-from" type="date" label="${escapeAttr(t('reports_from'))}" value="${escapeAttr(rep.from)}"></tf-input>
      <tf-input id="ps-rep-to" type="date" label="${escapeAttr(t('reports_to'))}" value="${escapeAttr(rep.to)}"></tf-input>
      <tf-select id="ps-rep-suite" label="${escapeAttr(t('reports_suite'))}" value="${escapeAttr(rep.suiteId)}">
        <option value="" ${rep.suiteId ? '' : 'selected'}>${escapeHtml(t('reports_suite_all'))}</option>
        ${s.suites.map((su) => {
          const sid = fv(su, 'suite_id');
          return `<option value="${escapeAttr(sid)}" ${sid === rep.suiteId ? 'selected' : ''}>${escapeHtml(su.name)}</option>`;
        }).join('')}
      </tf-select>
      <tf-button variant="primary" icon="chart-line" id="ps-rep-run">${escapeHtml(t('reports_generate'))}</tf-button>
      <span class="ps-toolbar-spacer"></span>
      <tf-button variant="ghost" icon="file-text" id="ps-rep-print">${escapeHtml(t('reports_print'))}</tf-button>
    </div>
    <div id="ps-reports-grid" class="ps-reports-grid">
      ${rep.loaded ? '' : `<div class="ps-field-hint">${escapeHtml(t('reports_intro'))}</div>`}
    </div>
  `;

  byId('ps-rep-from')?.addEventListener('change', (e) => { rep.from = String(e.target.value ?? ''); });
  byId('ps-rep-to')?.addEventListener('change', (e) => { rep.to = String(e.target.value ?? ''); });
  byId('ps-rep-suite')?.addEventListener('change', (e) => { rep.suiteId = e.detail?.value ?? ''; });
  byId('ps-rep-run')?.addEventListener('click', () => loadReports());
  byId('ps-rep-print')?.addEventListener('click', () => window.print());

  if (rep.loaded) renderReportSections();
}

async function loadReports() {
  const s = f2();
  const rep = s.reports;
  const grid = byId('ps-reports-grid');
  if (grid) grid.innerHTML = `<div class="ps-loading"><tf-spinner size="sm"></tf-spinner> ${escapeHtml(t('reports_loading'))}</div>`;
  const results = await Promise.allSettled(REPORT_KINDS.map((kind) => ApiBinary.one('projectStudioReportQueryRequest', {
    projectId: projectId(),
    report: kind,
    fromDate: rep.from,
    toDate: rep.to,
    suiteId: rep.suiteId,
  })));
  rep.data = {};
  results.forEach((res, i) => {
    const kind = REPORT_KINDS[i];
    if (res.status === 'fulfilled') {
      let rows = [];
      try {
        const parsed = JSON.parse(fv(res.value, 'rows_json') || '[]');
        rows = Array.isArray(parsed) ? parsed : [];
      } catch { rows = []; }
      rep.data[kind] = { rows, error: null };
    } else {
      rep.data[kind] = { rows: [], error: res.reason?.message || String(res.reason) };
    }
  });
  rep.loaded = true;
  if (state.tab === 'tests' && s.view === 'reports') renderReportSections();
}

function reportSectionShell(kind, icon, chartId) {
  return `
    <tf-section-card class="ps-report-card" title="${escapeAttr(t(`report_${kind}_title`))}" icon="${icon}">
      <span slot="actions">
        <tf-button variant="ghost" size="sm" icon="download" data-report-csv="${kind}" title="${escapeAttr(t('reports_export_csv'))}"></tf-button>
      </span>
      <div id="${chartId}" class="ps-report-body"></div>
    </tf-section-card>
  `;
}

function renderReportSections() {
  const rep = f2().reports;
  const grid = byId('ps-reports-grid');
  if (!grid) return;
  grid.innerHTML = `
    ${reportSectionShell('runs_over_time', 'chart-line', 'ps-rep-trend')}
    ${reportSectionShell('suite_pass_rate', 'bar-chart', 'ps-rep-results')}
    ${reportSectionShell('tester_stats', 'users', 'ps-rep-testers')}
    ${reportSectionShell('source_coverage', 'database', 'ps-rep-coverage')}
    ${reportSectionShell('defects', 'alert', 'ps-rep-defects')}
  `;
  // renderReportSections may run twice on the same grid element (initial view
  // + after "Generate") — wire the CSV delegate only once.
  if (!grid.dataset.csvWired) {
    grid.dataset.csvWired = '1';
    grid.addEventListener('click', (e) => {
      const btn = e.target.closest('[data-report-csv]');
      if (!btn) return;
      const kind = btn.dataset.reportCsv;
      const data = f2().reports.data[kind];
      if (!data || !data.rows.length) {
        toast(t('reports_no_data'), 'info');
        return;
      }
      downloadTextFile(`report-${kind}.csv`, toCsv(data.rows));
    });
  }

  const emptyOrError = (kind, hostEl) => {
    const data = rep.data[kind] || { rows: [], error: null };
    if (data.error) {
      hostEl.innerHTML = `<div class="ps-form-error">${escapeHtml(data.error)}</div>`;
      return null;
    }
    if (!data.rows.length) {
      hostEl.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('reports_no_data'))}</div>`;
      return null;
    }
    return data.rows;
  };

  // 1) Trend — pass-rate line over time.
  const trendHost = byId('ps-rep-trend');
  const trendRows = emptyOrError('runs_over_time', trendHost);
  if (trendRows) {
    const points = trendRows.map((row) => {
      const x = String(rowVal(row, ['date', 'day', 'bucket', 'label'], ''));
      // Backend rows carry per-status counts only — derive the total here.
      const passed = Number(rowVal(row, ['passed'], 0));
      const total = ['passed', 'failed', 'blocked', 'skipped']
        .reduce((sum, key) => sum + Number(rowVal(row, [key], 0)), 0);
      const y = total > 0 ? Math.round((passed / total) * 1000) / 10 : 0;
      return { x, y };
    });
    const chart = document.createElement('tf-line-chart');
    chart.height = 220;
    chart.xAxis = { scale: 'category', min: null, max: null, ticks: null, format: null };
    chart.yAxis = { scale: 'linear', min: 0, max: 100, ticks: 5, format: (v) => `${v}%` };
    chart.series = [{
      id: 'pass_rate', name: t('report_pass_rate_series'), tone: 'success', style: 'solid', showInLegend: true, points,
    }];
    trendHost.replaceChildren(chart);
  }

  // 2) Results — stacked bar per suite/run bucket.
  const resultsHost = byId('ps-rep-results');
  const resultRows = emptyOrError('suite_pass_rate', resultsHost);
  if (resultRows) {
    const statuses = [
      { key: 'passed', tone: 'success' },
      { key: 'failed', tone: 'critical' },
      { key: 'blocked', tone: 'warning' },
      { key: 'skipped', tone: 'muted' },
    ];
    const catOf = (row) => String(rowVal(row, ['suite_name', 'suite', 'run_no', 'name', 'label', 'date'], '—'));
    const chart = document.createElement('tf-bar-chart');
    chart.height = 240;
    chart.stacking = 'stacked';
    chart.xAxis = { scale: 'category', min: null, max: null, ticks: null, format: null };
    chart.yAxis = { scale: 'linear', min: null, max: null, ticks: 5, format: null };
    chart.series = statuses.map((st) => ({
      id: st.key,
      name: t(`item_status_${st.key}`),
      tone: st.tone,
      style: 'solid',
      showInLegend: true,
      points: resultRows.map((row) => ({ x: catOf(row), y: Number(rowVal(row, [st.key], 0)) })),
    }));
    resultsHost.replaceChildren(chart);
  }

  // 3) Tester stats table.
  const testersHost = byId('ps-rep-testers');
  const testerRows = emptyOrError('tester_stats', testersHost);
  if (testerRows) {
    testersHost.innerHTML = `
      <tf-table>
        <tf-column key="tester" label="${escapeAttr(t('report_col_tester'))}"></tf-column>
        <tf-column key="done" label="${escapeAttr(t('report_col_done'))}" renderer="num"></tf-column>
        <tf-column key="passed" label="${escapeAttr(t('item_status_passed'))}" renderer="num"></tf-column>
        <tf-column key="failed" label="${escapeAttr(t('item_status_failed'))}" renderer="num"></tf-column>
        <tf-column key="blocked" label="${escapeAttr(t('item_status_blocked'))}" renderer="num"></tf-column>
        <tf-column key="avg" label="${escapeAttr(t('report_col_avg_time'))}"></tf-column>
      </tf-table>
    `;
    const table = testersHost.querySelector('tf-table');
    table.rows = testerRows.map((row) => ({
      tester: String(rowVal(row, ['tester_name', 'tester', 'display_name', 'name'], '—')),
      done: Number(rowVal(row, ['executed'], 0)),
      passed: Number(rowVal(row, ['passed'], 0)),
      failed: Number(rowVal(row, ['failed'], 0)),
      blocked: Number(rowVal(row, ['blocked'], 0)),
      avg: formatDuration(Number(rowVal(row, ['avg_duration_secs', 'avg_secs', 'avg_duration'], 0))),
    }));
  }

  // 4) Source coverage — custom rows so uncovered sources can be highlighted.
  const coverageHost = byId('ps-rep-coverage');
  const coverageRows = emptyOrError('source_coverage', coverageHost);
  if (coverageRows) {
    coverageHost.innerHTML = `
      <div class="ps-coverage-head">
        <span>${escapeHtml(t('report_col_source'))}</span>
        <span>${escapeHtml(t('report_col_cases'))}</span>
        <span>${escapeHtml(t('report_col_approved'))}</span>
      </div>
      ${coverageRows.map((row) => {
        const cases = Number(rowVal(row, ['cases_total'], 0));
        const approved = Number(rowVal(row, ['cases_approved'], 0));
        return `
          <div class="ps-coverage-row ${cases === 0 ? 'is-uncovered' : ''}">
            <span class="ps-coverage-name">${escapeHtml(String(rowVal(row, ['source_name', 'source', 'name'], '—')))}</span>
            <span>${cases === 0 ? `<tf-chip status="err">${escapeHtml(t('report_uncovered'))}</tf-chip>` : cases}</span>
            <span>${cases === 0 ? '—' : approved}</span>
          </div>
        `;
      }).join('')}
    `;
  }

  // 5) Defects — pie by severity.
  const defectsHost = byId('ps-rep-defects');
  const defectRows = emptyOrError('defects', defectsHost);
  if (defectRows) {
    const tones = { low: 'info', medium: 'primary', high: 'warning', critical: 'critical' };
    // Backend emits one row per (severity, status) pair — fold counts into a
    // single slice per severity so the pie has no duplicate segments.
    const bySeverity = new Map();
    for (const row of defectRows) {
      const severity = String(rowVal(row, ['severity'], '')) || 'medium';
      bySeverity.set(severity, (bySeverity.get(severity) || 0) + Number(rowVal(row, ['count'], 0)));
    }
    const slices = [...bySeverity.entries()]
      .map(([severity, value]) => ({
        id: severity,
        label: t(`sev_${severity}`),
        value,
        tone: tones[severity] || 'neutral',
      }))
      .filter((slice) => slice.value > 0);
    if (!slices.length) {
      defectsHost.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('reports_no_data'))}</div>`;
    } else {
      const chart = document.createElement('tf-pie-chart');
      chart.variant = 'donut';
      chart.height = 220;
      chart.showLegend = true;
      chart.showLabels = true;
      chart.slices = slices;
      defectsHost.replaceChildren(chart);
    }
  }
}

// =============================================================================
// Z01 — tasks & defects list
// =============================================================================

function taskNoLabel(task) {
  return `${task.task_type === 'defect' || fv(task, 'task_type') === 'defect' ? 'D' : 'T'}-${fv(task, 'task_no')}`;
}

async function loadTasksPage() {
  const tv = state.tasksView || (state.tasksView = freshTasksState());
  const resp = await ApiBinary.one('projectStudioTasksListRequest', {
    projectId: projectId(),
    taskType: tv.filters.type,
    status: tv.filters.status,
    assignedTo: tv.filters.mine ? 'me' : '',
    search: tv.filters.search,
    offset: (tv.page - 1) * F2_PAGE_SIZE,
    limit: F2_PAGE_SIZE,
  });
  tv.rows = Array.isArray(resp.tasks) ? resp.tasks : [];
  tv.total = Number(resp.total ?? tv.rows.length);
}

async function renderTasksTab() {
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  const tv = state.tasksView || (state.tasksView = freshTasksState());
  await ensureF2Members();
  try {
    await loadTasksPage();
  } catch (err) {
    panel.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('tasks_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tasks') return;

  panel.innerHTML = `
    <div class="ps-tests-toolbar">
      <tf-searchbox id="ps-tasks-search" placeholder="${escapeAttr(t('tasks_search_placeholder'))}" debounce="300" value="${escapeAttr(tv.filters.search)}"></tf-searchbox>
      <tf-segmented id="ps-tasks-f-type" value="${escapeAttr(tv.filters.type || 'all')}">
        <option value="all">${escapeHtml(t('tasks_filter_all'))}</option>
        <option value="task">${escapeHtml(t('task_type_task'))}</option>
        <option value="defect">${escapeHtml(t('task_type_defect'))}</option>
      </tf-segmented>
      <tf-select id="ps-tasks-f-status" value="${escapeAttr(tv.filters.status)}">
        <option value="" ${tv.filters.status === '' ? 'selected' : ''}>${escapeHtml(t('tasks_filter_status_all'))}</option>
        ${TASK_STATUSES.map((x) => `<option value="${x}" ${x === tv.filters.status ? 'selected' : ''}>${escapeHtml(t(`task_status_${x}`))}</option>`).join('')}
      </tf-select>
      <div class="ps-toggle-inline">
        <tf-toggle id="ps-tasks-f-mine" ${tv.filters.mine ? 'checked' : ''}></tf-toggle>
        <span>${escapeHtml(t('tasks_filter_mine'))}</span>
      </div>
      <span class="ps-toolbar-spacer"></span>
      ${canTest() ? `<tf-button variant="primary" icon="plus" id="ps-tasks-new">${escapeHtml(t('tasks_new'))}</tf-button>` : ''}
    </div>
    <div id="ps-tasks-table-host">
      ${tv.rows.length ? '' : `<tf-empty-state icon="check" title="${escapeAttr(t('tasks_empty'))}"></tf-empty-state>`}
    </div>
  `;

  const reload = () => { tv.page = 1; renderTasksTab(); };
  byId('ps-tasks-search')?.addEventListener('search', (e) => { tv.filters.search = String(e.detail?.value ?? ''); reload(); });
  byId('ps-tasks-f-type')?.addEventListener('change', (e) => {
    const v = e.detail?.value ?? 'all';
    tv.filters.type = v === 'all' ? '' : v;
    reload();
  });
  byId('ps-tasks-f-status')?.addEventListener('change', (e) => { tv.filters.status = e.detail?.value ?? e.target.value ?? ''; reload(); });
  byId('ps-tasks-f-mine')?.addEventListener('change', (e) => { tv.filters.mine = !!(e.detail?.checked ?? e.target.checked); reload(); });
  byId('ps-tasks-new')?.addEventListener('click', () => openTaskWindow({}));

  if (!tv.rows.length) return;
  byId('ps-tasks-table-host').innerHTML = `
    <tf-table id="ps-tasks-table" page-size="${F2_PAGE_SIZE}" total="${tv.total}" page="${tv.page}">
      <tf-column key="no" label="#"></tf-column>
      <tf-column key="title" label="${escapeAttr(t('tasks_col_title'))}"></tf-column>
      <tf-column key="type" label="${escapeAttr(t('tasks_col_type'))}" renderer="chip"></tf-column>
      <tf-column key="severity" label="${escapeAttr(t('tasks_col_severity'))}" renderer="chip"></tf-column>
      <tf-column key="priority" label="${escapeAttr(t('tasks_col_priority'))}" renderer="chip"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('tasks_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="assignee" label="${escapeAttr(t('tasks_col_assignee'))}"></tf-column>
      <tf-column key="due" label="${escapeAttr(t('tasks_col_due'))}"></tf-column>
      <tf-column key="comments" label="${escapeAttr(t('tasks_col_comments'))}" renderer="num"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-tasks-table');
  const assignRows = () => {
    table.rows = tv.rows.map((task) => {
      const type = fv(task, 'task_type');
      return {
        _id: fv(task, 'task_id'),
        no: taskNoLabel(task),
        title: task.title,
        type: chipCell(type === 'defect' ? 'err' : 'info', t(`task_type_${type}`)),
        severity: task.severity
          ? chipCell(PRIORITY_CHIP[task.severity], t(`sev_${task.severity}`))
          : chipCell('info', '—'),
        priority: chipCell(PRIORITY_CHIP[task.priority], t(`prio_${task.priority}`)),
        status: chipCell(TASK_STATUS_CHIP[task.status], t(`task_status_${task.status}`)),
        assignee: fv(task, 'assigned_to_name') || '—',
        due: fv(task, 'due_date') || '—',
        comments: Number(fv(task, 'comment_count') ?? 0),
      };
    });
  };
  assignRows();
  table.addEventListener('row-click', (e) => {
    const taskId = e.detail?.row?._id;
    if (taskId) openTaskWindow({ taskId });
  });
  table.addEventListener('page-change', async (e) => {
    tv.page = Number(e.detail?.page ?? 1);
    try {
      await loadTasksPage();
    } catch (err) {
      toast(`${t('tasks_failed')}: ${err.message}`, 'error');
      return;
    }
    table.setAttribute('page', String(tv.page));
    table.setAttribute('total', String(tv.total));
    assignRows();
  });
}

// =============================================================================
// Z02 — task / defect window (detail + comments)
// =============================================================================

// opts: { taskId } opens an existing task; without taskId the window is a
// creation form ({ taskType?, links?, titlePrefill? } — the tester desk
// prefills defect links).
async function openTaskWindow(opts = {}) {
  const tw = {
    taskId: opts.taskId || null,
    taskType: opts.taskType || 'task',
    title: opts.titlePrefill || '',
    descriptionMd: '',
    severity: '',
    priority: 'medium',
    status: 'todo',
    assignedTo: '',
    dueDate: '',
    links: Array.isArray(opts.links) ? opts.links.slice() : [],
    attachments: [],
    comments: [],
    info: null,
    busy: false,
  };
  await ensureF2Members();
  if (tw.taskId) {
    try {
      const resp = await ApiBinary.one('projectStudioTaskGetRequest', { projectId: projectId(), taskId: tw.taskId });
      const detail = resp.detail || {};
      const info = detail.info || {};
      tw.info = info;
      tw.taskType = fv(info, 'task_type') || 'task';
      tw.title = info.title || '';
      tw.descriptionMd = fv(detail, 'description_md') || '';
      tw.severity = info.severity || '';
      tw.priority = info.priority || 'medium';
      tw.status = info.status || 'todo';
      tw.assignedTo = fv(info, 'assigned_to') || '';
      tw.dueDate = fv(info, 'due_date') || '';
      try {
        const links = JSON.parse(fv(info, 'links_json') || '[]');
        tw.links = Array.isArray(links) ? links : [];
      } catch { tw.links = []; }
      tw.attachments = Array.isArray(detail.attachments) ? detail.attachments.slice() : [];
      tw.comments = Array.isArray(detail.comments) ? detail.comments.slice() : [];
    } catch (err) {
      toast(`${t('task_load_failed')}: ${err.message}`, 'error');
      return;
    }
  }

  const isAuthor = tw.info ? isMe(fv(tw.info, 'created_by')) : true;
  const mayEdit = canEdit() || isAuthor || isMe(tw.assignedTo);
  const mayDelete = tw.taskId && (canManage() || isAuthor);

  const { body, foot, cleanup } = openWindow({
    title: tw.taskId ? t('task_win_title', { no: taskNoLabel(tw.info) }) : t('task_win_new_title'),
    subtitle: tw.info ? `${fv(tw.info, 'created_by_name') || ''} · ${formatTimestamp(fv(tw.info, 'created_at'))}` : '',
    icon: tw.taskType === 'defect' ? 'alert' : 'check',
    width: 760,
  });

  const testers = testerMembers();
  body.innerHTML = `
    <div class="ps-field" style="margin-bottom:12px;">
      <span class="ps-field-label">${escapeHtml(t('task_type_label'))}</span>
      <tf-segmented id="ps-task-type" value="${escapeAttr(tw.taskType)}" ${mayEdit ? '' : 'disabled'}>
        <option value="task">${escapeHtml(t('task_type_task'))}</option>
        <option value="defect">${escapeHtml(t('task_type_defect'))}</option>
      </tf-segmented>
    </div>
    <div class="ps-field" style="margin-bottom:12px;">
      <tf-input id="ps-task-title" label="${escapeAttr(t('task_title_label'))}" value="${escapeAttr(tw.title)}" ${mayEdit ? '' : 'readonly'}></tf-input>
    </div>
    <div class="ps-field" style="margin-bottom:12px;">
      <tf-textarea id="ps-task-desc" rows="4" label="${escapeAttr(t('task_desc_label'))}" hint="${escapeAttr(t('task_desc_hint'))}" ${mayEdit ? '' : 'readonly'}></tf-textarea>
    </div>
    <div class="ps-task-grid">
      <tf-select id="ps-task-severity" label="${escapeAttr(t('task_severity_label'))}" value="${escapeAttr(tw.severity)}" ${mayEdit ? '' : 'disabled'}>
        <option value="" ${tw.severity === '' ? 'selected' : ''}>—</option>
        ${PRIORITIES.map((x) => `<option value="${x}" ${x === tw.severity ? 'selected' : ''}>${escapeHtml(t(`sev_${x}`))}</option>`).join('')}
      </tf-select>
      <tf-select id="ps-task-priority" label="${escapeAttr(t('task_priority_label'))}" value="${escapeAttr(tw.priority)}" ${mayEdit ? '' : 'disabled'}>
        ${PRIORITIES.map((x) => `<option value="${x}" ${x === tw.priority ? 'selected' : ''}>${escapeHtml(t(`prio_${x}`))}</option>`).join('')}
      </tf-select>
      <tf-select id="ps-task-status" label="${escapeAttr(t('task_status_label'))}" value="${escapeAttr(tw.status)}" ${mayEdit ? '' : 'disabled'}>
        ${TASK_STATUSES.map((x) => `<option value="${x}" ${x === tw.status ? 'selected' : ''}>${escapeHtml(t(`task_status_${x}`))}</option>`).join('')}
      </tf-select>
      <tf-select id="ps-task-assignee" label="${escapeAttr(t('task_assignee_label'))}" value="${escapeAttr(tw.assignedTo)}" ${mayEdit ? '' : 'disabled'}>
        <option value="" ${tw.assignedTo === '' ? 'selected' : ''}>${escapeHtml(t('task_unassigned'))}</option>
        ${testers.map((m) => `<option value="${escapeAttr(fv(m, 'user_id'))}" ${fv(m, 'user_id') === tw.assignedTo ? 'selected' : ''}>${escapeHtml(fv(m, 'display_name') || '')}</option>`).join('')}
      </tf-select>
      <tf-input id="ps-task-due" type="date" label="${escapeAttr(t('task_due_label'))}" value="${escapeAttr(tw.dueDate)}" ${mayEdit ? '' : 'readonly'}></tf-input>
    </div>
    <div class="ps-field" style="margin-bottom:12px;">
      <span class="ps-field-label">${escapeHtml(t('task_links_label'))}</span>
      <div class="ps-task-links" id="ps-task-links"></div>
    </div>
    <div class="ps-field" style="margin-bottom:12px;">
      <span class="ps-field-label">${escapeHtml(t('case_attachments_title'))}</span>
      <div id="ps-task-atts"></div>
      ${mayEdit ? `<tf-file-input id="ps-task-att-input" multiple label="${escapeAttr(t('attachment_dropzone'))}"></tf-file-input>` : ''}
    </div>
    ${tw.taskId ? `
      <div class="ps-field">
        <span class="ps-field-label">${escapeHtml(t('task_comments_label', { count: tw.comments.length }))}</span>
        <div class="ps-task-comments" id="ps-task-comments"></div>
        ${canTest() ? `
          <div class="ps-task-comment-add">
            <tf-textarea id="ps-task-comment-input" rows="2" placeholder="${escapeAttr(t('task_comment_placeholder'))}"></tf-textarea>
            <tf-button variant="ghost" icon="send" id="ps-task-comment-send">${escapeHtml(t('task_comment_send'))}</tf-button>
          </div>
        ` : ''}
      </div>
    ` : ''}
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left">
      ${mayDelete ? `<tf-button variant="danger-solid" icon="trash" data-action="delete">${escapeHtml(t('action_delete'))}</tf-button>` : ''}
    </div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      ${mayEdit ? `<tf-button variant="primary" icon="check" data-action="save">${escapeHtml(t('action_save'))}</tf-button>` : ''}
    </div>
  `;

  const descEl = body.querySelector('#ps-task-desc');
  if (descEl) descEl.value = tw.descriptionMd;
  const showError = (msg) => {
    const el = body.querySelector('[data-form-error]');
    if (el) { el.hidden = !msg; el.textContent = msg || ''; }
  };

  const renderLinks = () => {
    const hostEl = body.querySelector('#ps-task-links');
    if (!hostEl) return;
    hostEl.innerHTML = tw.links.length ? tw.links.map((link, i) => `
      <tf-chip status="accent" ${mayEdit ? 'removable' : ''} data-task-link="${i}">
        ${escapeHtml(t(`link_kind_${link.kind}`) )}: ${escapeHtml(link.label || link.id || '')}
      </tf-chip>
    `).join('') : `<span class="ps-field-hint">${escapeHtml(t('task_links_empty'))}</span>`;
    hostEl.querySelectorAll('[data-task-link]').forEach((chipEl) => {
      chipEl.addEventListener('remove', () => {
        tw.links.splice(Number(chipEl.dataset.taskLink), 1);
        renderLinks();
      });
    });
  };

  const renderAtts = () => {
    const hostEl = body.querySelector('#ps-task-atts');
    if (!hostEl) return;
    hostEl.innerHTML = tw.attachments.length
      ? tw.attachments.map((att, i) => attachmentRowHtml(att, i, { removable: mayEdit })).join('')
      : `<div class="ps-field-hint">${escapeHtml(t('attachments_empty'))}</div>`;
    hostEl.querySelectorAll('[data-att-preview]').forEach((btn) => {
      btn.addEventListener('click', () => openAttachmentPreview(tw.attachments[Number(btn.dataset.attPreview)]));
    });
    hostEl.querySelectorAll('[data-att-remove]').forEach((btn) => {
      btn.addEventListener('click', () => {
        tw.attachments.splice(Number(btn.dataset.attRemove), 1);
        renderAtts();
      });
    });
  };

  const renderComments = () => {
    const hostEl = body.querySelector('#ps-task-comments');
    if (!hostEl) return;
    hostEl.innerHTML = tw.comments.length ? tw.comments.map((c, i) => {
      const own = isMe(fv(c, 'author_user_id'));
      return `
        <div class="ps-task-comment">
          <div class="ps-av-mini">${escapeHtml(initials(fv(c, 'author_name')))}</div>
          <div class="ps-task-comment-main">
            <div class="ps-task-comment-head">
              <b>${escapeHtml(fv(c, 'author_name') || '')}</b>
              <span>${escapeHtml(formatTimestamp(fv(c, 'created_at')))}${fv(c, 'edited_at') ? ` · ${escapeHtml(t('task_comment_edited'))}` : ''}</span>
            </div>
            <div class="ps-task-comment-body">${escapeHtml(fv(c, 'body_md') || '')}</div>
          </div>
          <div class="ps-task-comment-actions">
            ${own ? `<tf-button variant="ghost" size="sm" icon="edit" data-comment-edit="${i}" title="${escapeAttr(t('action_edit'))}"></tf-button>` : ''}
            ${own || canManage() ? `<tf-button variant="ghost" size="sm" icon="trash" data-comment-delete="${i}" title="${escapeAttr(t('action_delete'))}"></tf-button>` : ''}
          </div>
        </div>
      `;
    }).join('') : `<div class="ps-field-hint">${escapeHtml(t('task_comments_empty'))}</div>`;

    hostEl.querySelectorAll('[data-comment-edit]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        const c = tw.comments[Number(btn.dataset.commentEdit)];
        if (!c) return;
        const next = await openPromptWindow({
          title: t('task_comment_edit_title'),
          label: t('task_comment_placeholder'),
          value: fv(c, 'body_md') || '',
        });
        if (next == null || !next) return;
        try {
          await ApiBinary.one('projectStudioTaskCommentEditRequest', {
            projectId: projectId(), commentId: fv(c, 'comment_id'), bodyMd: next,
          });
          c.body_md = next;
          c.edited_at = new Date().toISOString();
          renderComments();
        } catch (err) {
          toast(`${t('task_comment_failed')}: ${err.message}`, 'error');
        }
      });
    });
    hostEl.querySelectorAll('[data-comment-delete]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        const c = tw.comments[Number(btn.dataset.commentDelete)];
        if (!c) return;
        const ok = await TfWindow.confirm({
          title: t('task_comment_delete_title'),
          message: t('task_comment_delete_message'),
          confirmLabel: t('action_delete'),
          cancelLabel: t('action_cancel'),
          danger: true,
        });
        if (!ok) return;
        try {
          await ApiBinary.one('projectStudioTaskCommentDeleteRequest', {
            projectId: projectId(), commentId: fv(c, 'comment_id'),
          });
          tw.comments.splice(Number(btn.dataset.commentDelete), 1);
          renderComments();
        } catch (err) {
          toast(`${t('task_comment_failed')}: ${err.message}`, 'error');
        }
      });
    });
  };

  body.querySelector('#ps-task-type')?.addEventListener('change', (e) => { tw.taskType = e.detail?.value ?? tw.taskType; });
  body.querySelector('#ps-task-title')?.addEventListener('input', (e) => { tw.title = String(e.target.value ?? ''); });
  descEl?.addEventListener('input', () => { tw.descriptionMd = String(descEl.value ?? ''); });
  body.querySelector('#ps-task-severity')?.addEventListener('change', (e) => { tw.severity = e.detail?.value ?? ''; });
  body.querySelector('#ps-task-priority')?.addEventListener('change', (e) => { tw.priority = e.detail?.value ?? tw.priority; });
  body.querySelector('#ps-task-status')?.addEventListener('change', (e) => { tw.status = e.detail?.value ?? tw.status; });
  body.querySelector('#ps-task-assignee')?.addEventListener('change', (e) => { tw.assignedTo = e.detail?.value ?? ''; });
  body.querySelector('#ps-task-due')?.addEventListener('change', (e) => { tw.dueDate = String(e.target.value ?? ''); });
  body.querySelector('#ps-task-att-input')?.addEventListener('change', async (e) => {
    const files = e.detail?.files;
    if (!files || !files.length) return;
    try {
      for (const file of Array.from(files)) {
        const att = await uploadAttachmentFile(file);
        if (!tw.attachments.some((a) => fv(a, 'sha256') === att.sha256)) tw.attachments.push(att);
      }
      renderAtts();
      toast(t('attachment_uploaded'), 'success');
    } catch (err) {
      toast(`${t('attachment_upload_failed')}: ${err.message}`, 'error');
    }
  });
  body.querySelector('#ps-task-comment-send')?.addEventListener('click', async () => {
    const input = body.querySelector('#ps-task-comment-input');
    const text = String(input?.value ?? '').trim();
    if (!text) return;
    try {
      const resp = await ApiBinary.one('projectStudioTaskCommentAddRequest', {
        projectId: projectId(), taskId: tw.taskId, bodyMd: text,
      });
      if (resp.comment) tw.comments.push(resp.comment);
      if (input) input.value = '';
      renderComments();
    } catch (err) {
      toast(`${t('task_comment_failed')}: ${err.message}`, 'error');
    }
  });

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || tw.busy) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    if (btn.dataset.action === 'delete') {
      const ok = await TfWindow.confirm({
        title: t('task_delete_title'),
        message: t('task_delete_message', { title: escapeHtml(tw.title) }),
        confirmLabel: t('action_delete'),
        cancelLabel: t('action_cancel'),
        danger: true,
      });
      if (!ok) return;
      try {
        await ApiBinary.one('projectStudioTaskDeleteRequest', { projectId: projectId(), taskId: tw.taskId });
        toast(t('task_deleted'), 'success');
        cleanup();
        if (state.tab === 'tasks') await renderTasksTab();
      } catch (err) {
        showError(`${t('task_delete_failed')}: ${err.message}`);
      }
      return;
    }
    // Save.
    const title = tw.title.trim();
    if (title.length < 3) { showError(t('err_task_title')); return; }
    if (tw.taskType === 'defect' && !tw.severity) { showError(t('err_task_severity')); return; }
    tw.busy = true;
    btn.setAttribute('disabled', '');
    try {
      const resp = await ApiBinary.one('projectStudioTaskSaveRequest', {
        projectId: projectId(),
        taskId: tw.taskId,
        taskType: tw.taskType,
        title,
        descriptionMd: tw.descriptionMd,
        severity: tw.taskType === 'defect' ? tw.severity : '',
        priority: tw.priority,
        status: tw.status,
        assignedTo: tw.assignedTo,
        dueDate: tw.dueDate,
        linksJson: JSON.stringify(tw.links),
        attachmentsJson: JSON.stringify(tw.attachments),
      });
      toast(t('task_saved', { no: `${tw.taskType === 'defect' ? 'D' : 'T'}-${Number(fv(resp, 'task_no') ?? 0)}` }), 'success');
      cleanup();
      if (state.tab === 'tasks') await renderTasksTab();
    } catch (err) {
      tw.busy = false;
      btn.removeAttribute('disabled');
      showError(`${t('task_save_failed')}: ${err.message}`);
    }
  });

  renderLinks();
  renderAtts();
  if (tw.taskId) renderComments();
}

// =============================================================================
// G02 — notification bell, panel and cross-project "my test work"
// =============================================================================

// The unsolicited SystemEvent listener survives module remounts by design —
// the client connection is page-scoped and the server already filters
// UserNotification frames per authenticated user.
let notifListenerInstalled = false;

function bellHtml() {
  return `
    <span class="ps-bell-wrap" data-bell-wrap>
      <tf-button variant="ghost" icon="mail" data-bell title="${escapeAttr(t('notif_title'))}"></tf-button>
      <span class="ps-bell-badge" data-bell-badge hidden></span>
    </span>
  `;
}

function wireBellEvents(root) {
  root?.querySelectorAll('[data-bell]').forEach((btn) => {
    btn.addEventListener('click', () => openNotifWindow());
  });
}

function updateBellBadges() {
  document.querySelectorAll('[data-bell-badge]').forEach((badge) => {
    const count = state.notifUnread;
    badge.hidden = count <= 0;
    badge.textContent = count > 99 ? '99+' : String(count);
  });
}

async function refreshNotifBadge() {
  try {
    const resp = await ApiBinary.one('projectStudioNotificationsListRequest', { onlyUnread: true, limit: 1 });
    state.notifUnread = Number(fv(resp, 'unread_count') ?? 0);
  } catch {
    // Backend not ready / offline — the badge simply stays as-is.
    return;
  }
  updateBellBadges();
}

function installNotifListener() {
  if (notifListenerInstalled) return;
  notifListenerInstalled = true;
  ApiBinary.client()
    .then((client) => {
      client.addUnsolicitedListener(({ body }) => {
        if (!body || body.variant !== 'UserNotification') return;
        // ws_binary already forwards the frame only to this user's connections;
        // the payload is display-ready (title + body composed server-side).
        toast(`${body.title || t('notif_title')}${body.body ? ` — ${body.body}` : ''}`, 'info');
        refreshNotifBadge();
      });
    })
    .catch(() => { /* transport not ready; badge refresh covers it later */ });
}

async function openNotifWindow() {
  const { body, foot, cleanup } = openWindow({
    title: t('notif_title'),
    icon: 'mail',
    width: 520,
  });
  const nw = { items: [], hasMore: false, loading: false };

  body.innerHTML = `<div class="ps-notif-list" id="ps-notif-list"><div class="ps-loading">${escapeHtml(t('loading'))}</div></div>`;
  foot.innerHTML = `
    <div class="ps-footer-left">
      <tf-button variant="ghost" size="sm" icon="check" data-action="mark-all">${escapeHtml(t('notif_mark_all'))}</tf-button>
    </div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" size="sm" icon="list" data-action="my-work">${escapeHtml(t('notif_my_work'))}</tf-button>
      <tf-button variant="ghost" size="sm" data-action="close-notif">${escapeHtml(t('action_close'))}</tf-button>
    </div>
  `;

  const listEl = body.querySelector('#ps-notif-list');

  const renderList = () => {
    if (!nw.items.length) {
      listEl.innerHTML = `<tf-empty-state icon="mail" title="${escapeAttr(t('notif_empty'))}"></tf-empty-state>`;
      return;
    }
    listEl.innerHTML = `
      ${nw.items.map((n, i) => {
        const unread = !fv(n, 'read_at');
        return `
          <div class="ps-notif-item ${unread ? 'is-unread' : ''}" data-notif="${i}" role="button" tabindex="0">
            <div class="ps-notif-ico">${sprite(NOTIF_KIND_ICON[n.kind] || 'info')}</div>
            <div class="ps-notif-main">
              <div class="ps-notif-title">${escapeHtml(n.title || t(`nk_${n.kind}`))}</div>
              <div class="ps-notif-body">${escapeHtml(n.body || '')}</div>
              <div class="ps-notif-meta">${escapeHtml(fv(n, 'project_name') || '')} · ${escapeHtml(formatTimestamp(fv(n, 'created_at')))}</div>
            </div>
            ${unread ? '<span class="ps-notif-dot"></span>' : ''}
          </div>
        `;
      }).join('')}
      ${nw.hasMore ? `<div class="ps-notif-more"><tf-button variant="ghost" size="sm" icon="chevron-down" data-notif-more>${escapeHtml(t('notif_load_more'))}</tf-button></div>` : ''}
    `;
  };

  const loadPage = async (beforeId) => {
    if (nw.loading) return;
    nw.loading = true;
    try {
      const resp = await ApiBinary.one('projectStudioNotificationsListRequest', {
        onlyUnread: false, beforeId: beforeId || null, limit: 30,
      });
      const rows = Array.isArray(resp.notifications) ? resp.notifications : [];
      nw.items = beforeId ? nw.items.concat(rows) : rows;
      nw.hasMore = !!fv(resp, 'has_more');
      state.notifUnread = Number(fv(resp, 'unread_count') ?? state.notifUnread);
      updateBellBadges();
      renderList();
    } catch (err) {
      listEl.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('notif_failed')}: ${err.message}`)}</div>`;
    } finally {
      nw.loading = false;
    }
  };

  listEl.addEventListener('click', async (e) => {
    const more = e.target.closest('[data-notif-more]');
    if (more) {
      const last = nw.items[nw.items.length - 1];
      if (last) loadPage(fv(last, 'notification_id'));
      return;
    }
    const item = e.target.closest('[data-notif]');
    if (!item) return;
    const n = nw.items[Number(item.dataset.notif)];
    if (!n) return;
    if (!fv(n, 'read_at')) {
      try {
        await ApiBinary.one('projectStudioNotificationsMarkReadRequest', { notificationIds: [fv(n, 'notification_id')] });
        n.read_at = new Date().toISOString();
        state.notifUnread = Math.max(0, state.notifUnread - 1);
        updateBellBadges();
        renderList();
      } catch { /* mark-read is best-effort; the list stays consistent on reload */ }
    }
    // Deep link: run / generation / task notifications navigate straight into
    // the run detail, the generation detail or the task window.
    let link = null;
    try { link = JSON.parse(fv(n, 'link_json') || 'null'); } catch { link = null; }
    const runId = link && (link.run_id || (link.kind === 'run' ? link.id : null));
    const genId = link && link.gen_id;
    const taskId = link && link.task_id;
    const pid = fv(n, 'project_id');
    if (!pid || !(runId || genId || taskId)) return;
    cleanup();
    const sameProject = state.project && projectId() === pid;
    if (runId) {
      if (sameProject) await openRunDetailFromNotif(runId);
      else await openProject(pid, { tab: 'tests', runId });
    } else if (genId) {
      if (sameProject) await openGenDetailFromNotif(genId);
      else await openProject(pid, { tab: 'tests', sub: 'generations', genId });
    } else if (sameProject) {
      await openTaskWindowFromNotif(taskId);
    } else {
      await openProject(pid, { tab: 'tasks', taskId });
    }
  });

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'close-notif') { cleanup(); return; }
    if (btn.dataset.action === 'my-work') { cleanup(); openMyWorkWindow(); return; }
    if (btn.dataset.action === 'mark-all') {
      try {
        await ApiBinary.one('projectStudioNotificationsMarkReadRequest', { notificationIds: [] });
        nw.items.forEach((n) => { if (!fv(n, 'read_at')) n.read_at = new Date().toISOString(); });
        state.notifUnread = 0;
        updateBellBadges();
        renderList();
      } catch (err) {
        toast(`${t('notif_failed')}: ${err.message}`, 'error');
      }
    }
  });

  await loadPage(null);
}

// Detail navigation from a notification inside the already-open project: make
// sure the target tab is active before drilling into the run / generation /
// task, mirroring the cross-project `openProject(pid, deep)` path.
async function openRunDetailFromNotif(runId) {
  if (state.tab !== 'tests') {
    state.tab = 'tests';
    renderTabsValue();
    await switchTab('tests');
  }
  await openRunDetail(runId);
}

async function openGenDetailFromNotif(genId) {
  if (state.tab !== 'tests') {
    state.tab = 'tests';
    renderTabsValue();
    await switchTab('tests');
  }
  await openGenDetail(genId);
}

async function openTaskWindowFromNotif(taskId) {
  if (state.tab !== 'tasks') {
    state.tab = 'tasks';
    renderTabsValue();
    await switchTab('tasks');
  }
  await openTaskWindow({ taskId });
}

async function openMyWorkWindow() {
  const { body, foot, cleanup } = openWindow({
    title: t('mywork_title'),
    subtitle: t('mywork_sub'),
    icon: 'list',
    width: 560,
  });
  body.innerHTML = `<div class="ps-loading">${escapeHtml(t('loading'))}</div>`;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" size="sm" data-action="close-mywork">${escapeHtml(t('action_close'))}</tf-button>
    </div>
  `;
  foot.addEventListener('click', (e) => {
    if (e.target.closest('[data-action="close-mywork"]')) cleanup();
  });

  let entries = [];
  try {
    const resp = await ApiBinary.one('projectStudioMyTestWorkRequest');
    entries = Array.isArray(resp.entries) ? resp.entries : [];
  } catch (err) {
    body.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('mywork_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (!entries.length) {
    body.innerHTML = `<tf-empty-state icon="check" title="${escapeAttr(t('mywork_empty'))}"></tf-empty-state>`;
    return;
  }
  body.innerHTML = entries.map((entry, i) => `
    <div class="ps-mywork-row" data-mywork="${i}" role="button" tabindex="0">
      <div class="ps-notif-ico">${sprite('play')}</div>
      <div class="ps-notif-main">
        <div class="ps-notif-title">#${fv(entry, 'run_no')} ${escapeHtml(fv(entry, 'run_name') || '')}</div>
        <div class="ps-notif-meta">${escapeHtml(fv(entry, 'project_name') || '')}</div>
      </div>
      <div class="ps-mywork-counts">
        <tf-chip status="info">${escapeHtml(t('mywork_pending', { count: Number(fv(entry, 'items_pending') ?? 0) }))}</tf-chip>
        ${Number(fv(entry, 'items_in_progress') ?? 0) > 0 ? `<tf-chip status="accent">${escapeHtml(t('mywork_in_progress', { count: Number(fv(entry, 'items_in_progress') ?? 0) }))}</tf-chip>` : ''}
      </div>
    </div>
  `).join('');
  body.addEventListener('click', async (e) => {
    const row = e.target.closest('[data-mywork]');
    if (!row) return;
    const entry = entries[Number(row.dataset.mywork)];
    if (!entry) return;
    cleanup();
    const pid = fv(entry, 'project_id');
    const runId = fv(entry, 'run_id');
    if (state.project && projectId() === pid) await openRunDetailFromNotif(runId);
    else await openProject(pid, { tab: 'tests', runId });
  });
}
