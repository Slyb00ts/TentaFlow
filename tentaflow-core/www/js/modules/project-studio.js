// ===== File: project-studio.js — Project Studio ("Projekty"): list, wizard, knowledge, chat, members, settings =====
//
// Phase-1 surface over MessageBody::ProjectStudioBody (binary protocol only,
// codec projectStudio* helpers). Screens: project list (P01), 3-step creation
// wizard (P02), project overview with KPI + activity (P03), knowledge sources
// with chunked upload and live ingest tracking (W01/W02), KB search (W03),
// source files with preview (W04), private per-user chat with citations (C01),
// members (X03), settings (X04) and the reusable danger delete window (G01).
// tf-* components only; every visible string comes from i18n project_studio.*.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast, formatBytes } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-toggle.js';
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
};

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
    await loadProjects();
  },

  unmount() {
    closeAllWindows();
    stopAllJobTracking();
    stopChatStream();
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

async function openProject(projectId) {
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
  state.project = project;
  state.tab = 'overview';
  state.kbView = 'sources';
  state.kbHits = null;
  state.kbQuery = '';
  state.kbSelectedSources = new Set();
  state.chats = [];
  state.chatId = null;
  state.chatMessages = [];

  const listView = byId('ps-list-view');
  const projectView = byId('ps-project-view');
  if (listView) listView.hidden = true;
  if (!projectView) return;
  projectView.hidden = false;
  renderProjectShell();
  await switchTab('overview');
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
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  panel.innerHTML = `<div class="ps-loading">${escapeHtml(t('loading'))}</div>`;
  switch (tab) {
    case 'overview': await renderOverview(); break;
    case 'knowledge': await renderKnowledge(); break;
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
  if (modules.includes('chat')) {
    quickActions.push({ id: 'chat', icon: 'message', name: t('qa_chat'), sub: t('qa_chat_sub') });
  }
  quickActions.push({ id: 'members', icon: 'users', name: t('qa_members'), sub: t('qa_members_sub') });

  panel.innerHTML = `
    <div class="ps-kpi-grid">
      <tf-stat-card icon="database" label="${escapeAttr(t('kpi_sources'))}" value="${sourcesReady}" suffix="/ ${sourcesTotal}"
        ${openJobs > 0 ? `delta="${escapeAttr(t('kpi_open_jobs', { count: openJobs }))}" delta-type="warn"` : ''}></tf-stat-card>
      <tf-stat-card icon="file-text" label="${escapeAttr(t('kpi_files'))}" value="${kpis?.files_total ?? kpis?.filesTotal ?? 0}"></tf-stat-card>
      <tf-stat-card icon="grid-2x2" label="${escapeAttr(t('kpi_chunks'))}" value="${kpis?.chunks_total ?? kpis?.chunksTotal ?? 0}"></tf-stat-card>
      <tf-stat-card icon="users" label="${escapeAttr(t('kpi_members'))}" value="${kpis?.member_count ?? kpis?.memberCount ?? 0}"
        delta="${escapeAttr(t('kpi_my_chats', { count: kpis?.my_chat_count ?? kpis?.myChatCount ?? 0 }))}" delta-type="neutral"></tf-stat-card>
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
