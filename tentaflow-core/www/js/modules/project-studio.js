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
// reports (T14), tasks/defects (Z01/Z02) and the notification bell (G02),
// plus Phase-3: test environments with the admin approval queue (T12), live
// automated/perf runs with artifacts (T10/T11), the code-case editor with AI
// assist and try-run (T03), git/ZIP/OpenAPI knowledge sources (W01/W02/W04)
// and code-kind generation with per-kind agents (T04/X04),
// plus Phase-4: run schedules (T13), ML Studio links (X02), the kanban task
// board (Z01), the performance / tester-activity reports (T14) and project
// export / import archives.
// tf-* components only; every visible string comes from i18n project_studio.*.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast, formatBytes } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { Router } from '/js/router.js';
import { TfWindow } from '/js/components/tf-window.js';
import { TfAgentActivity } from '/js/components/tf-agent-activity.js';
import { renderMarkdown } from '/js/lib/md-lite.js';
import { activityLabels } from '/js/lib/agent-activity-bridge.js';
import { openActionMenu, openConfirmWindow, openHandoverWindow, showUndoToast } from '/js/lib/actions/index.js';
import { openHandover } from '/js/modules/org-structure/handover-nav.js';
import { PROJECT_AREAS, PERMISSION_LEVELS, BUILTIN_FUNCTIONS, allowsArea, canCreateTask, projectTabs, catalogueLabel, memberGroup, expiryToInstant, expiryToLocal } from './project-studio-access.js';
import { TASK_LINK_KINDS, projectKeySuggestion, taskTypeLabel, taskTypeDescription, taskDuration, taskEventValue, taskEventReferences, taskEventAttachments, taskNotificationText, activeTaskTypes, parentCandidates } from './project-studio-tasks.js';
import { projectAncestors, projectRoots, projectTreeNodes, readTaskScope, writeTaskScope } from './project-studio-tree.js';
import { uploadProjectAttachment, pendingAttachmentUploads, cancelAttachmentUpload, refreshAttachmentUpload, acknowledgeAttachmentUploads, openAttachmentStream, downloadProjectAttachment } from './project-studio-media.js';
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
import '/js/components/tf-sparkline.js';
import '/js/components/tf-status-pill.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-kanban.js';
import '/js/components/tf-combobox.js';
import '/js/components/tf-person-picker.js';
import '/js/components/tf-timeline.js';
import '/js/components/tf-tree.js';
import '/js/components/tf-multiselect.js';
import '/js/components/tf-video-stream.js';

// Wizard templates map 1:1 to the wire `template` field; modules are the
// initial toggles for step 2 (knowledge is always locked on).
const TEMPLATES = [
  { id: 'custom', icon: 'folder', modules: ['knowledge'] },
  { id: 'tests', icon: 'list', modules: ['knowledge', 'tests', 'tasks'] },
  { id: 'docs', icon: 'file-text', modules: ['knowledge', 'docs', 'chat'] },
  { id: 'tests_docs', icon: 'grid-2x2', modules: ['knowledge', 'tests', 'docs', 'tasks', 'chat'] },
];

const CHILD_TEMPLATES = [
  { id: 'deployment', modules: ['tasks', 'tests', 'knowledge', 'docs', 'chat'] },
  { id: 'maintenance', modules: ['tasks', 'tests', 'knowledge', 'chat'] },
  { id: 'empty', modules: [] },
];

const MODULE_DEFS = [
  { id: 'knowledge', icon: 'database', locked: true },
  { id: 'tests', icon: 'list' },
  { id: 'docs', icon: 'file-text' },
  { id: 'chat', icon: 'message' },
  { id: 'tasks', icon: 'check' },
];

const SOURCE_KINDS = [
  { id: 'document', icon: 'file-text' },
  { id: 'url', icon: 'globe' },
  { id: 'git', icon: 'branch' },
  { id: 'zip', icon: 'folder' },
  { id: 'api_spec', icon: 'code' },
];

const SOURCE_KIND_ICON = { document: 'file-text', url: 'globe', git: 'branch', zip: 'folder', api_spec: 'code' };
const SOURCE_STATUS_CHIP = { pending: 'info', indexing: 'accent', ready: 'ok', error: 'err', cancelled: 'warn' };
const FILE_STATUS_CHIP = { pending: 'info', indexing: 'accent', ready: 'ok', skipped: 'warn', error: 'err' };

const UPLOAD_CHUNK_BYTES = 1024 * 1024;
const FILES_PAGE_SIZE = 50;
const INGEST_POLL_MS = 3000;
const INGEST_LOG_CAP = 200;

// ---- F3 constants ----------------------------------------------------------

const ENV_TYPES = ['web', 'api'];
const ENV_AUTH_TYPES = ['none', 'bearer', 'api_key', 'basic'];
const ENV_STATUS_CHIP = { pending: 'warn', approved: 'ok', rejected: 'err' };
// Client-side pre-check only: the server classifies the address authoritatively
// (it resolves the host), the UI just shows the approval banner up front.
const PRIVATE_HOST_RE = /^(localhost|.*\.local|.*\.internal|.*\.lan|127\.|10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.|169\.254\.|\[?::1\]?|\[?fc|\[?fd)/i;

// Languages the runner contract knows. Only 'python' is executable today
// (generation::CODE_LANGUAGES) — the rest is listed so the missing toolchain is
// visible instead of silently absent, exactly like the T03 mockup.
const CODE_LANGUAGES = [
  { id: 'python', executable: true },
  { id: 'javascript', executable: false },
  { id: 'typescript', executable: false },
];
const CODE_KINDS = ['ui', 'api', 'unit', 'perf', 'security'];
// Case kind -> project agent binding (mirrors generation::agent_function_for_kind).
const KIND_AGENT_FUNCTION = {
  ui: 'generator_ui',
  api: 'generator_api',
  unit: 'generator_unit',
  perf: 'generator_perf',
  security: 'security',
};
// Agent bindings the settings screen manages; 'chat' drives the project chat,
// the rest drives generation + code assist per case kind.
const AGENT_FUNCTIONS = ['chat', 'generator_ui', 'generator_api', 'generator_unit', 'generator_perf', 'security', 'critic', 'documentalist'];

const AUTO_RUN_POLL_MS = 3000;
const AUTO_LOG_CAP = 500;
const TRY_LOG_CAP = 300;
const ARTIFACT_MAX_BYTES = 32 * 1024 * 1024;
const PERF_DEFAULT_PROFILE = { users: 50, spawn_rate: 5, duration_secs: 60 };
const PERF_LIMITS = { users: [1, 2000], spawnRate: [0.1, 2000], duration: [5, 3600] };
const ARTIFACT_ICON = {
  log: 'file-text', screenshot: 'image', trace: 'branch', junit: 'list',
  perf_stats: 'chart-line', har: 'network', other: 'paperclip',
};

// ---- F4 constants ----------------------------------------------------------

// 'interval' = every N m/h/d, 'cron' = daily "minute hour * * *", 'once' =
// a single RFC3339 instant. next_run_at / next_runs_preview are ALWAYS taken
// from the server: recomputing them here would disagree with the firing loop
// around DST transitions.
const SCHEDULE_KINDS = ['interval', 'cron', 'once'];
const RUN_TYPES = ['manual', 'auto', 'perf'];
const SCHEDULE_OUTCOME_CHIP = { started: 'ok', skipped: 'info', blocked: 'warn', error: 'err' };
const SCHEDULE_LAST_CHIP = {
  running: 'accent', completed: 'ok', cancelled: 'warn', error: 'err',
  started: 'accent', skipped: 'info', blocked: 'warn',
};
const INTERVAL_RE = /^\d+[mhd]$/;
const DAILY_CRON_RE = /^([0-5]?\d)\s+([01]?\d|2[0-3])\s+\*\s+\*\s+\*$/;
const SCHEDULE_RUNS_LIMIT = 50;

const ML_PROJECT_LEVELS = ['read', 'write', 'admin'];
const ML_ROLES = ['editor', 'viewer'];
const ML_DEFAULT_ROLE_MAP = {
  read: 'viewer', write: 'editor', admin: 'editor',
};

// Board columns map 1:1 onto tasks.status; card order is NOT persisted in F4
// (sorted by priority then updated_at), so a drag only ever writes the column.
const TASK_BOARD_COLUMNS = [
  { id: 'todo', accent: 'info' },
  { id: 'in_progress', accent: 'accent' },
  { id: 'review', accent: 'warning' },
  { id: 'done', accent: 'success' },
];
const BOARD_PAGE_SIZE = 200;
const TASKS_VIEW_KEY = 'ps.tasks.view';

// Leave room for CBOR metadata inside the global 1 MiB WebSocket frame budget.
const IMPORT_CHUNK_BYTES = 512 * 1024;
const ARCHIVE_POLL_MS = 2000;
const ARCHIVE_LOG_CAP = 200;

const state = {
  me: null,
  isAdmin: false,
  // P01
  canCreate: false,
  projects: [],
  breadcrumbs: [],
  expandedProjects: new Set(),
  listFilter: 'active',
  functionFilter: 'all',
  catalogues: new Map(),
  listView: 'cards',
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
  kbError: '',
  files: { sourceId: null, offset: 0, filter: '', rows: [], total: 0, mode: 'tree' },
  // Chat
  chats: [],
  chatId: null,
  chatMessages: [],
  chatBusy: false,
  chatUnsub: null,
  // Members / settings
  members: [],
  functions: [],
  memberQuery: '',
  memberFunctionFilter: 'all',
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
  taskTypes: [],
  localAttachments: new Map(),
  // G02 — unread badge count (source of truth: NotificationsListRequest).
  notifUnread: 0,
  // F4 — X02 links tab ({ links, canManage, loaded }).
  connections: null,
  // F4 — live export/import job: { jobId, kind, unsub, pollTimer, log }.
  archiveJob: null,
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
    runs: { rows: [], total: 0, page: 1, status: '', type: '', filter: '' },
    runDetail: null,
    exec: null,
    gens: [],
    gensFilter: '',
    gensStatus: '',
    gensKind: '',
    genDetail: null,
    genUnsub: null,
    genPollTimer: null,
    genWidget: null,
    genSteps: [],
    // `perfSuiteId` / `perfEndpoint` scope the F4 performance card only; the
    // toolbar suite filter still drives every other report.
    reports: { from: '', to: '', suiteId: '', perfSuiteId: '', perfEndpoint: '', loaded: false, data: {} },
    // T13 — schedules; `serverTimezone` is the node zone the loop fires in.
    schedules: { rows: [], loaded: false, serverTimezone: '', kind: '', status: '', search: '' },
    // F3 — environments (T12), runner discovery and the live automated run
    // (T10/T11). `autoRun` owns its own stream + poll, torn down by
    // stopTestsLive() like every other live artifact of the tests tab.
    envs: { rows: [], loaded: false, type: '', status: '', filter: '' },
    envApprovals: { items: [], loaded: false },
    envPending: 0,
    runners: null,
    autoRun: null,
  };
}

function freshTasksState() {
  return {
    rows: [], total: 0, page: 1,
    filters: { type: '', status: '', severity: '', mine: false, search: '', includeArchived: false },
    // 'list' | 'board' — remembered per signed-in user and project.
    mode: 'list',
    // The board appends bounded pages and uses server totals for its counters.
    boardRows: [],
    scope: 'single', sourceProjectIds: [], summary: null,
    renderGeneration: 0,
  };
}

// localStorage is best-effort (private mode / disabled storage throws).
function readTasksViewMode(id) {
  try {
    const mode = window.localStorage.getItem(`${TASKS_VIEW_KEY}.${currentUserId()}.${id}`);
    return mode === 'board' ? 'board' : 'list';
  } catch {
    return 'list';
  }
}

function writeTasksViewMode(id, mode) {
  try {
    window.localStorage.setItem(`${TASKS_VIEW_KEY}.${currentUserId()}.${id}`, mode);
  } catch {
    /* storage unavailable — the mode simply does not survive a reload */
  }
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

function selectOpt(value, current, label) {
  return `<option value="${escapeAttr(value)}" ${value === current ? 'selected' : ''}>${escapeHtml(label)}</option>`;
}

function initials(name) {
  const parts = String(name || '').trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return '?';
  return parts.slice(0, 2).map((p) => p[0]).join('').toUpperCase();
}

// AuthMe user_id arrives as Uint8Array(16); project rows carry string ids
// (hex/uuid). Compare on the normalized hex form.
function currentUserId() {
  const bytes = state.me?.userId;
  return bytes ? Array.from(bytes).map((byte) => byte.toString(16).padStart(2, '0')).join('') : '';
}

function isMe(userId) {
  return !!userId && String(userId).toLowerCase().replace(/-/g, '') === currentUserId();
}

function projectAccess(project = state.project) {
  return project?.access || {};
}

const canArea = (area, minimum = 'read') => allowsArea(projectAccess(), area, minimum);
const canManageMembers = () => !!fv(projectAccess(), 'can_manage_members') && !projectAccess().archived && !projectAccess().ended;
const canTransferOwnership = () => !!fv(projectAccess(), 'is_owner') && !projectAccess().archived && !projectAccess().ended;
const canSendChat = () => canArea('chat', 'write') && canArea('knowledge');
const canTryRun = () => canArea('tests', 'write') && canArea('environments');
const canGenerateTests = () => canArea('tests', 'write') && canArea('knowledge');
const canRunSchedule = (schedule) => canArea('tests', 'write') && (fv(schedule, 'run_type') === 'manual' || canArea('environments'));
const canProject = (action, project = state.project) => project?.[`can_${action}`] === true;

function functionLabel(definition) {
  return catalogueLabel(definition, 'name', t);
}

function functionDescription(definition) {
  return catalogueLabel(definition, 'description', t);
}

function functionChips(functions, catalogue = state.functions) {
  if (!functions?.length) return `<span class="ps-field-hint">${escapeHtml(t('functions_none'))}</span>`;
  return functions.map((id) => {
    const definition = catalogue.find((entry) => fv(entry, 'function_id') === id);
    return `<tf-chip status="info">${escapeHtml(definition ? functionLabel(definition) : id)}</tf-chip>`;
  }).join(' ');
}

function accessSummary(project) {
  const access = projectAccess(project);
  if (fv(access, 'is_owner')) return t('access_owner');
  if (fv(access, 'project_admin')) return t('access_project_admin');
  if (fv(access, 'app_admin')) return t('access_app_admin');
  return t('access_member');
}

async function loadCatalogue(id) {
  const response = await ApiBinary.one('projectStudioCatalogueGetRequest', { projectId: id });
  const functions = Array.isArray(response.functions) ? response.functions : [];
  state.catalogues.set(id, functions);
  return functions;
}

function memberAccessFields(member, catalogue, suffix = '') {
  const assigned = member.functions || [];
  return `
    <div class="ps-access-form" data-access-form="${escapeAttr(suffix)}">
      <div class="ps-field-label">${escapeHtml(t('functions_label'))}</div>
      <div class="ps-function-choices">
        ${catalogue.map((definition) => `<tf-checkbox data-member-function="${escapeAttr(fv(definition, 'function_id'))}"
          label="${escapeAttr(functionLabel(definition))}" ${assigned.includes(fv(definition, 'function_id')) ? 'checked' : ''}></tf-checkbox>`).join('')}
      </div>
      <tf-checkbox data-project-admin label="${escapeAttr(t('access_project_admin'))}" ${fv(member, 'project_admin') ? 'checked' : ''}></tf-checkbox>
      <div class="ps-field-hint">${escapeHtml(t('access_admin_hint'))}</div>
      <tf-input data-member-expiry type="datetime-local" step="1" label="${escapeAttr(t('members_expiry'))}"
        value="${escapeAttr(expiryToLocal(fv(member, 'expires_at')))}" ${fv(member, 'is_owner') ? 'disabled' : ''}
        hint="${escapeAttr(t(fv(member, 'is_owner') ? 'members_expiry_owner_hint' : 'members_expiry_hint', { timezone: Intl.DateTimeFormat().resolvedOptions().timeZone }))}"></tf-input>
    </div>`;
}

function readMemberAccess(host) {
  let expiresAt;
  try { expiresAt = expiryToInstant(host.querySelector('[data-member-expiry]')?.value || ''); }
  catch { throw new Error(t('members_expiry_invalid')); }
  return {
    functions: [...host.querySelectorAll('[data-member-function]')].filter((item) => item.checked).map((item) => item.dataset.memberFunction),
    projectAdmin: host.querySelector('[data-project-admin]')?.checked === true,
    expiresAt,
  };
}

function orientationLabel(project) {
  const template = project.template;
  if (CHILD_TEMPLATES.some((entry) => entry.id === template)) return t(`subproject_template_${template}`);
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
            <tf-button variant="ghost" icon="cloud" id="ps-import" hidden>${escapeHtml(t('import_btn'))}</tf-button>
            <tf-button variant="primary" icon="plus" id="ps-new" hidden>${escapeHtml(t('new_project'))}</tf-button>
          </div>
        </div>
        <div class="ps-filters-row">
          <tf-filter-chips id="ps-filter" mode="single"></tf-filter-chips>
          <tf-select id="ps-function-filter" value="${escapeAttr(state.functionFilter)}">
            <option value="all">${escapeHtml(t('project_function_filter'))}</option>
            ${BUILTIN_FUNCTIONS.map((definition) => `<option value="${definition.functionId}">${escapeHtml(functionLabel(definition))}</option>`).join('')}
          </tf-select>
          <tf-segmented id="ps-view-mode" value="${escapeAttr(state.listView)}">
            <option value="cards" icon="apps">${escapeHtml(t('view_cards'))}</option>
            <option value="table" icon="list">${escapeHtml(t('view_table'))}</option>
          </tf-segmented>
          <span class="ps-toolbar-spacer"></span>
          <span class="ps-create-hint" id="ps-create-hint" hidden>${sprite('check')}${escapeHtml(t('can_create_hint'))}</span>
        </div>
        <div id="ps-grid-host"><div class="ps-loading">${escapeHtml(t('loading'))}</div></div>
      </div>
      <div id="ps-project-view" hidden></div>
    `;
  },

  async mount(params = {}) {
    const me = await ApiBinary.one('authMeRequest').catch(() => null);
    state.me = me;
    state.isAdmin = false;

    byId('ps-refresh')?.addEventListener('click', () => loadProjects());
    byId('ps-new')?.addEventListener('click', () => openWizard());
    byId('ps-import')?.addEventListener('click', () => openImportWindow());
    byId('ps-search')?.addEventListener('search', (e) => {
      state.searchQuery = String(e.detail?.value ?? '');
      renderProjectGrid();
    });
    const filter = byId('ps-filter');
    if (filter) {
      syncListFilterChips();
      filter.addEventListener('change', (e) => {
        state.listFilter = e.detail?.id ?? 'active';
        loadProjects();
      });
    }
    byId('ps-function-filter')?.addEventListener('change', (e) => {
      state.functionFilter = e.detail?.value ?? 'all';
      renderProjectGrid();
    });
    byId('ps-view-mode')?.addEventListener('change', (e) => {
      state.listView = e.detail?.value === 'table' ? 'table' : 'cards';
      renderProjectGrid();
    });
    wireGridEvents();
    wireBellEvents(byId('ps-list-view'));
    installNotifListener();
    refreshNotifBadge();
    await loadProjects();
    if (params.projectId) await openProject(params.projectId, params);
  },

  unmount() {
    closeAllWindows();
    stopAllJobTracking();
    stopChatStream();
    stopTestsLive();
    stopArchiveJob();
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
    state.functions = [];
    state.catalogues.clear();
    state.settings = null;
    state.searchQuery = '';
    state.listFilter = 'active';
    state.f2 = null;
    state.tasksView = null;
    state.taskTypes = [];
    state.localAttachments.clear();
    state.connections = null;
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
  const lifetime = new AbortController();
  const win = document.createElement('tf-window');
  win.classList.add('ps-project-window');
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
    lifetime.abort();
    if (win.isConnected) win.close(true);
    if (backdrop.isConnected) backdrop.remove();
    state.wins.delete(cleanup);
  };
  state.wins.add(cleanup);
  win.addEventListener('close-request', () => {
    lifetime.abort();
    if (backdrop.isConnected) backdrop.remove();
    state.wins.delete(cleanup);
  });
  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'close') cleanup();
  });

  return { win, body, foot, cleanup, signal: lifetime.signal };
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
// to retype the exact name before the destructive button unlocks. `extraHtml`
// adds one decision to the same window (e.g. "also revoke ML access"); the
// confirmation body is handed to `onConfirm` so it can read those controls.
function openDeleteWindow({ title, targetName, targetSub, targetIcon = 'folder', warning, items = [], requireName = null, extraHtml = '', confirmLabel, onConfirm }) {
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
    ${extraHtml}
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
      await onConfirm(body);
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
    const response = await ApiBinary.one('projectStudioProjectTreeRequest', { projectId: null, includeEnded: true, includeArchived: true });
    state.projects = response.projects;
    state.breadcrumbs = response.breadcrumbs;
    state.canCreate = response.can_create;
    state.isAdmin = response.can_administer;
    state.catalogues.clear();
    await Promise.allSettled(projectRoots(visibleProjects()).map((project) => loadCatalogue(project.project_id)));
    const choices = new Map(BUILTIN_FUNCTIONS.map((definition) => [definition.functionId, definition]));
    for (const catalogue of state.catalogues.values()) for (const definition of catalogue) choices.set(fv(definition, 'function_id'), definition);
    byId('ps-function-filter')?.setOptions([{ value: 'all', label: t('project_function_filter') }, ...[...choices].map(([value, definition]) => ({ value, label: functionLabel(definition) }))], state.functionFilter);
  } catch (error) { toast(`${t('load_failed')}: ${error.message}`, 'error'); return; }
  byId('ps-new').hidden = !state.canCreate;
  byId('ps-import').hidden = !state.canCreate;
  byId('ps-create-hint').hidden = !state.canCreate;
  const active = state.projects.filter((project) => project.status === 'active' && project.lifecycle === 'active').length;
  const archived = state.projects.filter((project) => project.status === 'archived').length;
  byId('ps-list-sub').textContent = t('subtitle_stats', { count: state.projects.length, active, archived });
  syncListFilterChips();
  renderProjectGrid();
}

// Chips carry live counts (mockup P01) so the operator sees the split without
// switching filters.
function syncListFilterChips() {
  const filter = byId('ps-filter');
  if (!filter) return;
  const active = state.projects.filter((p) => p.status === 'active' && p.lifecycle === 'active').length;
  const archived = state.projects.filter((p) => p.status === 'archived').length;
  filter.filters = [
    { id: 'active', label: `${t('filter_active')} ${active}`, active: state.listFilter === 'active' },
    { id: 'archived', label: `${t('filter_archived')} ${archived}`, active: state.listFilter === 'archived' },
    { id: 'ended', label: `${t('status_ended')} ${state.projects.filter((p) => p.lifecycle === 'ended').length}`, active: state.listFilter === 'ended' },
    { id: 'all', label: `${t('filter_all')} ${state.projects.length}`, active: state.listFilter === 'all' },
  ];
}

function visibleProjects() {
  const query = state.searchQuery.trim().toLowerCase();
  return state.projects.filter((p) => {
    if (state.listFilter === 'active' && (p.status !== 'active' || p.lifecycle !== 'active')) return false;
    if (state.listFilter === 'archived' && p.status !== 'archived') return false;
    if (state.listFilter === 'ended' && p.lifecycle !== 'ended') return false;
    if (state.functionFilter !== 'all' && !projectAccess(p).functions?.includes(state.functionFilter)) return false;
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
  const access = projectAccess(project);
  const memberCount = project.member_count ?? project.memberCount ?? 0;
  const sourceCount = project.source_count ?? project.sourceCount ?? 0;
  const sourcesReady = project.sources_ready ?? project.sourcesReady ?? 0;
  const menuItems = [
    `<tf-menu-item action="open" icon="external-link">${escapeHtml(t('action_open'))}</tf-menu-item>`,
  ];
  if (canProject(archived ? 'unarchive' : 'archive', project)) {
    menuItems.push(archived
      ? `<tf-menu-item action="unarchive" icon="refresh">${escapeHtml(t('action_unarchive'))}</tf-menu-item>`
      : `<tf-menu-item action="archive" icon="clock">${escapeHtml(t('action_archive'))}</tf-menu-item>`);
  }
  if (canProject('create_child', project)) menuItems.push(`<tf-menu-item action="child" icon="plus">${escapeHtml(t('subproject_new'))}</tf-menu-item>`);
  if (canProject('move', project)) menuItems.push(`<tf-menu-item action="move" icon="branch">${escapeHtml(t('subproject_move'))}</tf-menu-item>`);
  if (canProject('end', project) || canProject('resume', project)) menuItems.push(`<tf-menu-item action="lifecycle" icon="clock">${escapeHtml(t(project.lifecycle === 'ended' ? 'project_resume' : 'project_end'))}</tf-menu-item>`);
  if (canProject('delete', project)) {
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
        <tf-chip status="${archived || project.lifecycle === 'ended' ? 'warn' : 'ok'}" dot>${escapeHtml(t(archived ? 'status_archived' : project.lifecycle === 'ended' ? 'status_ended' : 'status_active'))}</tf-chip>
      </div>
      ${project.parent_id ? `<div class="ps-card-ancestors">${escapeHtml(projectAncestors(project, state.projects, state.breadcrumbs).map((row) => row.name).join(' / '))}</div>` : ''}
      <div class="ps-card-stats">
        <span class="ps-card-stat">${escapeHtml(t('tree_open_count', { own: project.own_open_tasks, descendants: project.descendant_open_tasks }))}</span>
        <span class="ps-card-stat">${escapeHtml(t('tree_my_count', { count: project.my_open_tasks }))}</span>
        <span class="ps-card-stat">${escapeHtml(t('tree_overdue_count', { count: project.overdue_tasks }))}</span>
        <span class="ps-card-stat">${sprite('users')}<b>${memberCount}</b>&nbsp;${escapeHtml(t('stat_members'))}</span>
        <span class="ps-card-stat">${sprite('database')}<b>${sourcesReady}/${sourceCount}</b>&nbsp;${escapeHtml(t('stat_sources'))}</span>
      </div>
      <div class="ps-card-foot">
        <tf-chip status="accent">${escapeHtml(accessSummary(project))}</tf-chip>
        ${functionChips(access.functions, state.catalogues.get(projectId) || [])}
        <tf-chip status="info">${escapeHtml(orientationLabel(project))}</tf-chip>
        ${state.projects.some((child) => child.parent_id === projectId) ? `<tf-button variant="ghost" size="sm" icon="${state.expandedProjects.has(projectId) ? 'chevron-up' : 'chevron-down'}" data-expand-project aria-expanded="${state.expandedProjects.has(projectId)}">${escapeHtml(t('tree_children_count', { count: state.projects.filter((child) => child.parent_id === projectId).length }))}</tf-button>` : ''}
        <div class="ps-card-menu-wrap">
          <tf-button variant="ghost" size="sm" icon="chevron-down" data-more title="${escapeAttr(t('action_more'))}"></tf-button>
          <tf-menu placement="bottom-end" data-card-menu>${menuItems.join('')}</tf-menu>
        </div>
      </div>
      ${state.expandedProjects.has(projectId) ? '<div data-card-children class="ps-project-tree"><tf-tree></tf-tree></div>' : ''}
    </div>
  `;
}

function renderProjectGrid() {
  const host = byId('ps-grid-host');
  if (!host) return;
  const visible = projectRoots(visibleProjects());
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
  if (state.listView === 'table') {
    renderProjectTable(host, visible);
    return;
  }
  host.innerHTML = `<div class="ps-grid">${visible.map(projectCardHtml).join('')}${addCard}</div>`;
  for (const project of visible) {
    const tree = host.querySelector(`[data-project-id="${CSS.escape(project.project_id)}"] [data-card-children] tf-tree`);
    if (tree) fillProjectTree(tree, descendantsOf(project.project_id, false));
  }
}

function renderProjectTable(host, visible) {
  host.innerHTML = `
    <tf-table id="ps-projects-table">
      <tf-column key="name" label="${escapeAttr(t('table_col_project'))}" renderer="html"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('table_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="functions" label="${escapeAttr(t('functions_label'))}" renderer="html"></tf-column>
      <tf-column key="modules" label="${escapeAttr(t('table_col_modules'))}"></tf-column>
      <tf-column key="members" label="${escapeAttr(t('stat_members'))}" renderer="num"></tf-column>
      <tf-column key="sources" label="${escapeAttr(t('stat_sources'))}"></tf-column>
      <tf-column key="updated" label="${escapeAttr(t('table_col_updated'))}"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-projects-table');
  table.rows = visible.map((p) => {
    const archived = p.status === 'archived';
    const modules = Array.isArray(p.modules) ? p.modules : [];
    return {
      _id: p.project_id ?? p.projectId,
      name: `<div class="tf-table__cell-title">${escapeHtml(p.name)}</div>`
        + `<div class="tf-table__cell-sub">${escapeHtml(p.description || '')}</div>`,
      status: chipCell(archived ? 'warn' : 'ok', t(archived ? 'status_archived' : 'status_active')),
      functions: `<tf-chip status="accent">${escapeHtml(accessSummary(p))}</tf-chip> ${functionChips(projectAccess(p).functions, state.catalogues.get(fv(p, 'project_id')) || [])}`,
      modules: modules.map((m) => t(`module_${m}`)).join(', ') || '—',
      members: Number(p.member_count ?? p.memberCount ?? 0),
      sources: `${p.sources_ready ?? p.sourcesReady ?? 0}/${p.source_count ?? p.sourceCount ?? 0}`,
      updated: formatTimestamp(p.updated_at ?? p.updatedAt),
    };
  });
  table.addEventListener('row-click', (e) => {
    const id = e.detail?.row?._id;
    if (id) openProject(id);
  });
}

// One delegated listener survives every grid re-render (tf-menu requires the
// menus to be statically present in each card's HTML).
function wireGridEvents() {
  const host = byId('ps-grid-host');
  if (!host) return;

  host.addEventListener('click', (e) => {
    const expand = e.target.closest('[data-expand-project]');
    if (expand) {
      e.stopPropagation();
      const id = expand.closest('[data-project-id]').dataset.projectId;
      if (state.expandedProjects.has(id)) state.expandedProjects.delete(id); else state.expandedProjects.add(id);
      renderProjectGrid(); return;
    }
    if (e.target.closest('[data-card-children]')) return;
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
      case 'child': if (project) openSubprojectWindow(project); break;
      case 'move': if (project) openProjectMoveWindow(project); break;
      case 'lifecycle': if (project) openProjectLifecycleWindow(project); break;
      case 'delete': if (project) confirmDeleteProject(project, { fromList: true }); break;
      default: break;
    }
  });
}

function setProjectArchived(projectId, archived) {
  const project = state.projects.find((row) => row.project_id === projectId) || (state.project?.project_id === projectId ? state.project : null);
  if (!canProject(archived ? 'archive' : 'unarchive', project)) return;
  openProjectArchiveWindow(project, archived);
}

function confirmDeleteProject(project, { fromList = false } = {}) {
  const projectId = project.project_id ?? project.projectId;
  const sourceCount = project.source_count ?? project.sourceCount ?? 0;
  const memberCount = project.member_count ?? project.memberCount ?? 0;
  openDeleteWindow({
    title: t('delete_project_title'),
    targetName: project.name,
    targetSub: t('delete_project_access_sub', { members: memberCount, access: accessSummary(project) }),
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

function descendantsOf(id, includeSelf = true) {
  const project = state.projects.find((row) => row.project_id === id) || (state.project?.project_id === id ? state.project : null);
  if (!project) return [];
  return state.projects.filter((row) => includeSelf && row.project_id === id || row.path.startsWith(`${project.path}/`));
}

async function switchProject(id) {
  if (id === projectId()) return;
  const tab = state.tab;
  const tasksMode = state.tasksView?.mode;
  closeAllWindows();
  await openProject(id, { tab, tasksMode });
  updateProjectRoute();
}

function projectTreeActions(project, anchor) {
  const items = [{ label: t('action_open'), icon: 'external-link', run: () => switchProject(project.project_id) }];
  if (allowsArea(project.access, 'settings', 'write')) items.push({ label: t('inheritance_title'), icon: 'settings', run: () => openProjectInheritanceWindow(project) });
  if (canProject('create_child', project)) items.push({ label: t('subproject_new'), icon: 'plus', run: () => openSubprojectWindow(project) });
  if (canProject('move', project)) items.push({ label: t('subproject_move'), icon: 'branch', run: () => openProjectMoveWindow(project) });
  if (canProject('end', project) || canProject('resume', project)) items.push({ label: t(project.lifecycle === 'ended' ? 'project_resume' : 'project_end'), icon: 'clock', run: () => openProjectLifecycleWindow(project) });
  if (canProject(project.status === 'archived' ? 'unarchive' : 'archive', project)) items.push({ label: t(project.status === 'archived' ? 'action_unarchive' : 'action_archive'), icon: 'archive', run: () => setProjectArchived(project.project_id, project.status !== 'archived') });
  if (canProject('delete', project)) items.push({ label: t('action_delete'), icon: 'trash', danger: true, run: () => confirmDeleteProject(project, { fromList: state.project?.project_id !== project.project_id }) });
  openActionMenu(anchor, items, project.name);
}

function fillProjectTree(tree, projects, { onSelect, allowMove = false, breadcrumbs = state.breadcrumbs } = {}) {
  tree.nodes = projectTreeNodes(projects, breadcrumbs, (project) => {
    const actions = document.createElement('tf-button');
    actions.setAttribute('variant', 'ghost'); actions.setAttribute('size', 'sm'); actions.setAttribute('icon', 'more'); actions.setAttribute('title', t('action_more'));
    actions.addEventListener('click', (event) => { event.stopPropagation(); projectTreeActions(project, actions); });
    return { actions, draggable: allowMove && canProject('move', project), droppable: allowMove && project.depth < 4 && project.access.is_owner && !project.access.archived && !project.access.ended,
      badge: { text: t('tree_open_count', { own: project.own_open_tasks, descendants: project.descendant_open_tasks }), tone: 'info' } };
  });
  const expanded = [];
  const expand = (nodes) => { for (const node of nodes) if (node.children.length) { expanded.push(node.id); expand(node.children); } };
  expand(tree.nodes);
  tree.expandedIds = expanded;
  tree.addEventListener('expand', (event) => { tree.expandedIds = [...new Set([...tree.expandedIds, event.detail.id])]; });
  tree.addEventListener('collapse', (event) => { tree.expandedIds = [...tree.expandedIds].filter((id) => id !== event.detail.id); });
  tree.addEventListener('select', (event) => {
    const project = projects.find((row) => row.project_id === event.detail.id);
    if (project) (onSelect || ((row) => switchProject(row.project_id)))(project);
  });
  tree.addEventListener('move', (event) => {
    const source = projects.find((row) => row.project_id === event.detail.id);
    if (source) openProjectMoveWindow(source, event.detail.parentId);
  });
}

function openProjectPicker() {
  const { body, foot, cleanup } = openWindow({ title: t('tree_picker'), icon: 'folder', width: 820 });
  body.innerHTML = `<tf-searchbox data-tree-search placeholder="${escapeAttr(t('tree_search'))}" debounce="200"></tf-searchbox><div class="ps-tree-filters"><tf-checkbox data-tree-my label="${escapeAttr(t('tree_my_only'))}"></tf-checkbox><tf-checkbox data-tree-open label="${escapeAttr(t('tree_open_only'))}"></tf-checkbox><tf-checkbox data-tree-due label="${escapeAttr(t('tree_overdue_only'))}"></tf-checkbox><tf-checkbox data-tree-ended label="${escapeAttr(t('tree_show_ended'))}"></tf-checkbox></div><div data-picker-tree class="ps-project-tree"></div>`;
  foot.innerHTML = `<div></div><tf-button variant="ghost" data-close>${escapeHtml(t('action_close'))}</tf-button>`;
  foot.querySelector('[data-close]').addEventListener('click', cleanup);
  const render = () => {
    const query = body.querySelector('[data-tree-search]').value.trim().toLowerCase();
    const projects = state.projects.filter((project) => (!query || `${project.name} ${project.description}`.toLowerCase().includes(query)) && (!body.querySelector('[data-tree-my]').checked || project.my_open_tasks > 0) && (!body.querySelector('[data-tree-open]').checked || project.own_open_tasks + project.descendant_open_tasks > 0) && (!body.querySelector('[data-tree-due]').checked || project.overdue_tasks > 0) && (body.querySelector('[data-tree-ended]').checked || project.lifecycle !== 'ended'));
    const host = body.querySelector('[data-picker-tree]');
    host.innerHTML = projects.length ? '<tf-tree></tf-tree>' : `<tf-empty-state icon="folder" title="${escapeAttr(t('tree_empty'))}"></tf-empty-state>`;
    if (projects.length) {
      const filteredIds = new Set(projects.map((project) => project.project_id));
      const ancestors = state.projects.filter((project) => !filteredIds.has(project.project_id) && projects.some((child) => child.path.startsWith(`${project.path}/`))).map(({ project_id, name, depth }) => ({ project_id, name, depth }));
      fillProjectTree(host.querySelector('tf-tree'), projects, { breadcrumbs: [...state.breadcrumbs, ...ancestors], onSelect: async (project) => { cleanup(); await switchProject(project.project_id); } });
    }
  };
  body.querySelector('[data-tree-search]').addEventListener('search', render);
  body.querySelectorAll('.ps-tree-filters tf-checkbox').forEach((control) => control.addEventListener('change', render));
  render();
}

function moduleChips(modules) {
  return modules.length ? modules.map((id) => `<tf-chip status="info">${escapeHtml(t(`module_${id}`))}</tf-chip>`).join(' ') : '—';
}

function openSubprojectWindow(parent) {
  if (!canProject('create_child', parent)) return;
  const { body, foot, cleanup } = openWindow({ title: t('subproject_new'), subtitle: parent.name, icon: 'branch', width: 720 });
  body.innerHTML = `<tf-input data-child-name label="${escapeAttr(t('wizard_name_label'))}"></tf-input><tf-input data-child-prefix label="${escapeAttr(t('project_key_prefix'))}" maxlength="8" hint="${escapeAttr(t('project_key_prefix_hint'))}"></tf-input><tf-textarea data-child-desc rows="3" label="${escapeAttr(t('wizard_desc_label'))}"></tf-textarea><tf-select data-child-template label="${escapeAttr(t('wizard_template_label'))}" value="deployment">${CHILD_TEMPLATES.map((preset) => `<option value="${preset.id}">${escapeHtml(t(`subproject_template_${preset.id}`))}</option>`).join('')}</tf-select><span data-template-hint class="ps-field-hint"></span><tf-checkbox data-child-inherit-modules checked label="${escapeAttr(t('inherit_modules'))}"></tf-checkbox><tf-checkbox data-child-inherit-types checked label="${escapeAttr(t('inherit_task_types'))}"></tf-checkbox><div data-child-effective class="ps-module-preview"></div><tf-checkbox data-child-private label="${escapeAttr(t('subproject_private'))}"></tf-checkbox><span class="ps-field-hint">${escapeHtml(t('subproject_private_hint'))}</span><span class="ps-field-hint">${escapeHtml(t('inheritance_members_hint'))}</span><div data-form-error class="ps-form-error" hidden></div>`;
  foot.innerHTML = `<div></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" icon="plus" data-action="save">${escapeHtml(t('wizard_create'))}</tf-button></div>`;
  let prefixEdited = false;
  body.querySelector('[data-child-template]').setOptions(CHILD_TEMPLATES.map((preset) => ({ value: preset.id, label: t(`subproject_template_${preset.id}`) })), 'deployment');
  body.querySelector('[data-child-prefix]').addEventListener('input', () => { prefixEdited = true; });
  body.querySelector('[data-child-name]').addEventListener('input', () => { if (!prefixEdited) body.querySelector('[data-child-prefix]').value = projectKeySuggestion(body.querySelector('[data-child-name]').value); });
  const sync = () => {
    const preset = CHILD_TEMPLATES.find((entry) => entry.id === body.querySelector('[data-child-template]').value);
    body.querySelector('[data-template-hint]').textContent = t(`subproject_template_${preset.id}_hint`);
    const inherited = body.querySelector('[data-child-inherit-modules]').checked;
    body.querySelector('[data-child-effective]').innerHTML = `<b>${escapeHtml(t(inherited ? 'inheritance_parent_modules' : 'subproject_own_preset'))}</b><div>${moduleChips(inherited ? parent.modules : preset.modules)}</div><span class="ps-field-hint">${escapeHtml(t(inherited ? 'inheritance_live' : 'inheritance_own'))}</span>`;
  };
  body.addEventListener('change', sync); sync();
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]');
    if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    const name = body.querySelector('[data-child-name]').value.trim();
    const keyPrefix = body.querySelector('[data-child-prefix]').value.trim().toUpperCase();
    const errorEl = body.querySelector('[data-form-error]');
    if (name.length < 3 || !/^[A-Z][A-Z0-9]{1,7}$/.test(keyPrefix)) { errorEl.hidden = false; errorEl.textContent = t(name.length < 3 ? 'err_name_short' : 'err_project_key_prefix'); return; }
    button.setAttribute('disabled', '');
    const preset = CHILD_TEMPLATES.find((entry) => entry.id === body.querySelector('[data-child-template]').value);
    try {
      const result = await ApiBinary.one('projectStudioProjectCreateRequest', { name, description: body.querySelector('[data-child-desc]').value.trim(), keyPrefix, template: preset.id, modules: preset.modules, parentId: parent.project_id, isPrivate: body.querySelector('[data-child-private]').checked, inheritModules: body.querySelector('[data-child-inherit-modules]').checked, inheritTaskTypes: body.querySelector('[data-child-inherit-types]').checked, members: [] });
      toast(t('subproject_created'), 'success'); cleanup(); await loadProjects(); await openProject(result.project_id);
    } catch (error) { errorEl.hidden = false; errorEl.textContent = `${t('create_failed')}: ${error.message}`; button.removeAttribute('disabled'); }
  });
}

function openProjectMoveWindow(project, targetId = null) {
  if (!canProject('move', project)) return;
  const height = Math.max(project.depth, ...descendantsOf(project.project_id).map((row) => row.depth)) - project.depth;
  const destinations = state.projects.filter((row) => row.access.is_owner && !row.access.archived && !row.access.ended && row.project_id !== project.project_id && !row.path.startsWith(`${project.path}/`) && row.depth + 1 + height <= 4);
  if (targetId && !destinations.some((row) => row.project_id === targetId)) { toast(t('subproject_move_invalid_target'), 'error'); return; }
  const { body, foot, cleanup } = openWindow({ title: t('subproject_move'), subtitle: project.name, icon: 'branch', width: 650 });
  body.innerHTML = `<div class="ps-banner-warn">${escapeHtml(t('subproject_move_hint'))}</div><tf-select data-move-parent label="${escapeAttr(t('subproject_parent'))}" value="${escapeAttr(targetId || '')}"><option value="">${escapeHtml(t('subproject_root'))}</option>${destinations.map((row) => `<option value="${escapeAttr(row.project_id)}" ${row.project_id === targetId ? 'selected' : ''}>${escapeHtml([...projectAncestors(row, state.projects, state.breadcrumbs).map((item) => item.name), row.name].join(' / '))}</option>`).join('')}</tf-select><div data-form-error class="ps-form-error" hidden></div>`;
  foot.innerHTML = `<div></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" icon="branch" data-action="save">${escapeHtml(t('subproject_move'))}</tf-button></div>`;
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]'); if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    button.setAttribute('disabled', '');
    try { await ApiBinary.one('projectStudioProjectMoveRequest', { projectId: project.project_id, newParentId: body.querySelector('[data-move-parent]').value || null }); toast(t('subproject_moved'), 'success'); cleanup(); await loadProjects(); if (state.project) await refreshProjectHeader(); }
    catch (error) { const el = body.querySelector('[data-form-error]'); el.hidden = false; el.textContent = error.message; button.removeAttribute('disabled'); }
  });
}

function renderSubprojectSettings(host) {
  if (!host) return;
  host.innerHTML = `<div class="ps-members-toolbar">${canProject('create_child') ? `<tf-button variant="primary" icon="plus" data-child-create>${escapeHtml(t('subproject_new'))}</tf-button>` : ''}${canProject('move') ? `<tf-button variant="ghost" icon="branch" data-child-move>${escapeHtml(t('subproject_move'))}</tf-button>` : ''}</div><div class="ps-subproject-layout"><div class="ps-project-tree"><tf-tree></tf-tree></div><div data-inheritance-inspector></div></div>`;
  host.querySelector('[data-child-create]')?.addEventListener('click', () => openSubprojectWindow(state.project));
  host.querySelector('[data-child-move]')?.addEventListener('click', () => openProjectMoveWindow(state.project));
  const inspector = host.querySelector('[data-inheritance-inspector]');
  fillProjectTree(host.querySelector('tf-tree'), descendantsOf(projectId()), { allowMove: true, onSelect: (project) => renderProjectInheritance(inspector, project) });
  renderProjectInheritance(inspector, state.project);
}

function openProjectInheritanceWindow(project) {
  const { body, foot, cleanup } = openWindow({ title: t('inheritance_title'), subtitle: project.name, icon: 'branch', width: 820 });
  foot.innerHTML = `<div></div><tf-button variant="ghost" data-close>${escapeHtml(t('action_close'))}</tf-button>`;
  foot.querySelector('[data-close]').addEventListener('click', cleanup);
  renderProjectInheritance(body, project);
}

function renderProjectInheritance(host, project) {
  const mutable = allowsArea(project.access, 'settings', 'write');
  host.classList.add('ps-inheritance-editor');
  host.innerHTML = `<h3>${escapeHtml(project.name)}</h3><span class="ps-field-hint">${escapeHtml(t('inheritance_members_hint'))}</span><tf-checkbox data-inheritance-private label="${escapeAttr(t('subproject_private'))}" ${project.is_private ? 'checked' : ''} ${mutable ? '' : 'disabled'}></tf-checkbox><span class="ps-field-hint">${escapeHtml(t('subproject_private_hint'))}</span><tf-checkbox data-inheritance-modules label="${escapeAttr(t('inherit_modules'))}" ${project.inherit_modules ? 'checked' : ''} ${mutable && project.parent_id ? '' : 'disabled'}></tf-checkbox><tf-checkbox data-inheritance-types label="${escapeAttr(t('inherit_task_types'))}" ${project.inherit_task_types ? 'checked' : ''} ${mutable && project.parent_id ? '' : 'disabled'}></tf-checkbox><div class="ps-members-toolbar">${mutable && project.parent_id ? `<tf-button variant="ghost" data-detach>${escapeHtml(t('inheritance_detach'))}</tf-button><tf-button variant="ghost" data-reinherit>${escapeHtml(t('inheritance_reinherit'))}</tf-button>` : ''}</div><div data-inheritance-diff></div>${mutable ? `<tf-button variant="primary" icon="check" data-inheritance-save disabled>${escapeHtml(t('action_save'))}</tf-button>` : ''}<div class="ps-form-error" data-form-error hidden></div>`;
  let generation = 0;
  let proposedModules = null;
  const showError = (error) => { const el = host.querySelector('[data-form-error]'); el.hidden = false; el.textContent = error.message; };
  const preview = async () => {
    const request = ++generation;
    proposedModules = null;
    const save = host.querySelector('[data-inheritance-save]'); save?.setAttribute('disabled', '');
    try {
      const result = await ApiBinary.one('projectStudioProjectInheritancePreviewRequest', { projectId: project.project_id, inheritModules: host.querySelector('[data-inheritance-modules]').checked, inheritTaskTypes: host.querySelector('[data-inheritance-types]').checked });
      if (!host.isConnected || request !== generation) return;
      proposedModules = result.proposed_modules;
      const types = (rows) => rows.map((row) => `${escapeHtml(taskTypeLabel(row, t))}${row.active ? '' : ` (${escapeHtml(t('task_type_inactive'))})`}`).join(', ') || '—';
      host.querySelector('[data-inheritance-diff]').innerHTML = `<div class="ps-inheritance-diff"><div><b>${escapeHtml(t('inheritance_diff_current'))}</b><div>${moduleChips(result.current_modules)}</div><p>${types(result.current_task_types)}</p></div><div><b>${escapeHtml(t('inheritance_diff_proposed'))}</b><div>${moduleChips(result.proposed_modules)}</div><p>${types(result.proposed_task_types)}</p></div></div><span class="ps-field-hint">${escapeHtml(t('inheritance_diff_hint'))}</span>`;
      save?.removeAttribute('disabled');
    } catch (error) { if (request === generation) showError(error); }
  };
  host.onchange = mutable ? preview : null;
  host.querySelector('[data-detach]')?.addEventListener('click', () => { host.querySelector('[data-inheritance-modules]').checked = false; host.querySelector('[data-inheritance-types]').checked = false; preview(); });
  host.querySelector('[data-reinherit]')?.addEventListener('click', () => { host.querySelector('[data-inheritance-modules]').checked = true; host.querySelector('[data-inheritance-types]').checked = true; preview(); });
  host.querySelector('[data-inheritance-save]')?.addEventListener('click', async (event) => {
    if (proposedModules === null) return;
    event.currentTarget.setAttribute('disabled', '');
    try {
      await ApiBinary.one('projectStudioProjectInheritanceSaveRequest', { projectId: project.project_id, isPrivate: host.querySelector('[data-inheritance-private]').checked, inheritModules: host.querySelector('[data-inheritance-modules]').checked, inheritTaskTypes: host.querySelector('[data-inheritance-types]').checked, modules: proposedModules });
      toast(t('inheritance_saved'), 'success'); await loadProjects();
      if (projectId() === project.project_id) await refreshProjectHeader();
      else { const current = state.projects.find((row) => row.project_id === project.project_id); if (current && host.isConnected) renderProjectInheritance(host, current); }
    } catch (error) { showError(error); event.currentTarget.removeAttribute('disabled'); }
  });
  if (mutable) preview();
  else host.querySelector('[data-inheritance-diff]').innerHTML = `<div class="ps-module-preview"><div>${moduleChips(project.modules)}</div><span class="ps-field-hint">${escapeHtml(t(project.inherit_modules || project.inherit_task_types ? 'inheritance_live' : 'inheritance_own'))}</span></div>`;
}

function openProjectArchiveWindow(project, archived) {
  const actionLabel = t(archived ? 'action_archive' : 'action_unarchive');
  const { body, foot, cleanup } = openWindow({ title: actionLabel, subtitle: project.name, icon: 'archive', width: 650 });
  body.innerHTML = `<tf-select data-archive-scope label="${escapeAttr(t('archive_scope'))}" value="node"><option value="node">${escapeHtml(t('scope_node'))}</option><option value="subtree">${escapeHtml(t('scope_subtree'))}</option></tf-select><div data-scope-nodes></div><div data-form-error class="ps-form-error" hidden></div>`;
  foot.innerHTML = `<div></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" icon="archive" data-action="archive" disabled>${escapeHtml(actionLabel)}</tf-button></div>`;
  let version = 0;
  const preview = async () => {
    const query = ++version; const button = foot.querySelector('[data-action="archive"]'); button.setAttribute('disabled', '');
    const errorElement = body.querySelector('[data-form-error]'); errorElement.hidden = true; errorElement.textContent = '';
    try { const result = await ApiBinary.one('projectStudioProjectScopePreviewRequest', { projectId: project.project_id, scope: body.querySelector('[data-archive-scope]').value, operation: 'archive' }); if (query !== version || !body.isConnected) return; body.querySelector('[data-scope-nodes]').innerHTML = scopeNodesHtml(result.nodes); button.removeAttribute('disabled'); }
    catch (error) { if (query !== version || !body.isConnected) return; errorElement.hidden = false; errorElement.textContent = error.message; }
  };
  body.querySelector('[data-archive-scope]').addEventListener('change', preview); preview();
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]'); if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    button.setAttribute('disabled', '');
    try { await ApiBinary.one('projectStudioProjectArchiveRequest', { projectId: project.project_id, archived, scope: body.querySelector('[data-archive-scope]').value }); toast(t(archived ? 'archive_ok' : 'unarchive_ok'), 'success'); cleanup(); await loadProjects(); if (state.project) await refreshProjectHeader(); }
    catch (error) { const el = body.querySelector('[data-form-error]'); el.hidden = false; el.textContent = error.message; button.removeAttribute('disabled'); }
  });
}

function scopeNodesHtml(nodes) {
  return `<b>${escapeHtml(t('scope_exact_nodes'))}</b><ul class="ps-scope-nodes">${nodes.map((row) => `<li>${escapeHtml(row.name)} <span class="ps-field-hint">${row.depth}</span></li>`).join('')}</ul>`;
}

async function openProjectLifecycleWindow(project) {
  const ended = project.lifecycle === 'ended';
  if (!canProject(ended ? 'resume' : 'end', project)) return;
  const { body, foot, cleanup } = openWindow({ title: t(ended ? 'project_resume' : 'project_lifecycle_title'), subtitle: project.name, icon: 'clock', width: 820 });
  body.innerHTML = `<div class="ps-loading">${escapeHtml(t('loading'))}</div>`;
  foot.innerHTML = `<div></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" data-action="apply" disabled>${escapeHtml(t(ended ? 'project_resume' : 'project_end'))}</tf-button></div>`;
  let preview;
  let taskStatus = 'todo';
  let taskPage = 1;
  let taskGeneration = 0;
  const loadTaskRows = async () => {
    const host = body.querySelector('[data-lifecycle-tasks]');
    if (!host) return;
    const generation = ++taskGeneration;
    try {
      const result = await ApiBinary.one('projectStudioTasksListRequest', { projectId: project.project_id, scope: 'single', sourceProjectIds: [], taskType: '', status: taskStatus, assignedTo: '', search: '', severity: '', includeArchived: false, offset: (taskPage - 1) * F2_PAGE_SIZE, limit: F2_PAGE_SIZE });
      if (!host.isConnected || generation !== taskGeneration) return;
      const lastPage = Math.max(1, Math.ceil(result.total / F2_PAGE_SIZE));
      if (taskPage > lastPage) { taskPage = lastPage; await loadTaskRows(); return; }
      host.innerHTML = `<tf-select data-lifecycle-status label="${escapeAttr(t('tasks_col_status'))}" value="${taskStatus}">${TASK_STATUSES.filter((status) => status !== 'done').map((status) => `<option value="${status}" ${status === taskStatus ? 'selected' : ''}>${escapeHtml(t(`task_status_${status}`))}</option>`).join('')}</tf-select><tf-table page-size="${F2_PAGE_SIZE}" page="${taskPage}" total="${result.total}"><tf-column key="key" label="#"></tf-column><tf-column key="title" label="${escapeAttr(t('tasks_col_title'))}"></tf-column><tf-column key="status" label="${escapeAttr(t('tasks_col_status'))}"></tf-column></tf-table>`;
      host.querySelector('[data-lifecycle-status]').addEventListener('change', (event) => { taskStatus = event.detail.value; taskPage = 1; loadTaskRows(); });
      const table = host.querySelector('tf-table');
      table.rows = result.tasks.map((task) => ({ _id: task.task_id, key: task.task_key, title: task.title, status: t(`task_status_${task.status}`) }));
      table.addEventListener('page-change', (event) => { taskPage = Number(event.detail.page); loadTaskRows(); });
      table.addEventListener('row-click', (event) => { cleanup(); openProject(project.project_id, { tab: 'tasks', taskId: event.detail.row._id }); });
      table.rowActions = (row) => {
        if (!allowsArea(project.access, 'tasks', 'write')) return null;
        const button = document.createElement('tf-button'); button.setAttribute('variant', 'ghost'); button.setAttribute('size', 'sm'); button.setAttribute('icon', 'more'); button.setAttribute('title', t('action_more'));
        const task = result.tasks.find((item) => item.task_id === row._id);
        button.addEventListener('click', (event) => { event.stopPropagation(); openActionMenu(button, [{ label: t('task_not_pursued'), icon: 'check', run: () => openTaskResolutionWindow(task, project, load) }, ...(project.access.is_owner ? [{ label: t('task_transfer'), icon: 'branch', run: () => openTaskTransferWindow(task, project, load) }] : [])], task.title); });
        return button;
      };
    } catch (error) { if (host.isConnected && generation === taskGeneration) host.innerHTML = `<div class="ps-form-error">${escapeHtml(error.message)}</div>`; }
  };
  const load = async () => {
    try {
      preview = await ApiBinary.one('projectStudioProjectLifecyclePreviewRequest', { projectId: project.project_id });
      if (!body.isConnected) return;
      body.innerHTML = `<div class="ps-banner-warn">${escapeHtml(t(ended ? 'project_resume_hint' : 'project_lifecycle_effect'))}</div><div class="ps-lifecycle-stats"><tf-stat-card label="${escapeAttr(t('project_lifecycle_open', { count: preview.open_task_count }))}" value="${preview.open_task_count}"></tf-stat-card><tf-stat-card label="${escapeAttr(t('project_lifecycle_members', { count: preview.exclusive_member_count }))}" value="${preview.exclusive_member_count}"></tf-stat-card></div>${preview.active_children.length ? `<b>${escapeHtml(t('project_lifecycle_children'))}</b><div class="ps-lifecycle-children">${preview.active_children.map((row) => `<tf-button variant="ghost" data-lifecycle-child="${escapeAttr(row.project_id)}">${escapeHtml(row.name)}</tf-button>`).join('')}</div>` : ''}${!ended && !preview.can_end ? `<div class="ps-banner-warn">${escapeHtml(t('project_lifecycle_blocked'))}</div><div data-lifecycle-tasks></div><tf-button variant="ghost" icon="refresh" data-lifecycle-refresh>${escapeHtml(t('refresh'))}</tf-button>` : ''}<tf-textarea data-lifecycle-reason label="${escapeAttr(t('project_lifecycle_reason'))}" rows="3"></tf-textarea><tf-input data-lifecycle-confirm label="${escapeAttr(t('project_lifecycle_confirm', { prefix: project.key_prefix }))}"></tf-input><div class="ps-form-error" data-form-error hidden></div>`;
      foot.querySelector('[data-action="apply"]').toggleAttribute('disabled', !(ended || preview.can_end));
      body.querySelectorAll('[data-lifecycle-child]').forEach((button) => button.addEventListener('click', () => { cleanup(); switchProject(button.dataset.lifecycleChild); }));
      body.querySelector('[data-lifecycle-refresh]')?.addEventListener('click', load);
      if (!ended && preview.open_task_count && (allowsArea(project.access, 'tasks') || allowsArea(project.access, 'board'))) {
        await loadTaskRows();
      }
    } catch (error) { body.innerHTML = `<div class="ps-form-error">${escapeHtml(error.message)}</div>`; }
  };
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]'); if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    const reason = body.querySelector('[data-lifecycle-reason]').value.trim();
    const confirmationKey = body.querySelector('[data-lifecycle-confirm]').value.trim();
    const errorEl = body.querySelector('[data-form-error]');
    if (!reason || confirmationKey !== project.key_prefix) { errorEl.hidden = false; errorEl.textContent = t(!reason ? 'project_lifecycle_reason' : 'project_lifecycle_confirm', { prefix: project.key_prefix }); return; }
    button.setAttribute('disabled', '');
    try { await ApiBinary.one('projectStudioProjectLifecycleRequest', { projectId: project.project_id, ended: !ended, confirmationKey, reason }); toast(t(ended ? 'project_resumed_ok' : 'project_ended_ok'), 'success'); cleanup(); await loadProjects(); if (state.project?.project_id === project.project_id) await refreshProjectHeader(); }
    catch (error) { errorEl.hidden = false; errorEl.textContent = error.message; button.removeAttribute('disabled'); }
  });
  await load();
}

function openTaskResolutionWindow(task, project = state.project, onSaved) {
  if (!allowsArea(project.access, 'tasks', 'write') || task.status === 'done' || task.archived_at) return;
  const { body, foot, cleanup } = openWindow({ title: t('task_not_pursued'), subtitle: `${task.task_key} · ${task.title}`, icon: 'check', width: 620 });
  body.innerHTML = `<tf-textarea data-resolution-reason label="${escapeAttr(t('task_resolution_reason'))}" rows="3"></tf-textarea><div class="ps-form-error" data-form-error hidden></div>`;
  foot.innerHTML = `<div></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" data-action="save">${escapeHtml(t('action_save'))}</tf-button></div>`;
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]'); if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    const reason = body.querySelector('[data-resolution-reason]').value.trim();
    const errorEl = body.querySelector('[data-form-error]');
    if (!reason) { errorEl.hidden = false; errorEl.textContent = t('task_resolution_reason'); return; }
    button.setAttribute('disabled', '');
    try { await ApiBinary.one('projectStudioTaskNotPursuedRequest', { projectId: project.project_id, taskId: task.task_id, reason }); toast(t('task_not_pursued_ok'), 'success'); cleanup(); if (onSaved) await onSaved(); else if (projectId() === project.project_id && state.tab === 'tasks') await renderTasksTab(); }
    catch (error) { errorEl.hidden = false; errorEl.textContent = error.message; button.removeAttribute('disabled'); }
  });
}

function openTaskTransferWindow(task, source = state.project, onSaved) {
  if (!source.access.is_owner || source.access.archived || source.access.ended || task.archived_at) return;
  const destinations = state.projects.filter((project) => project.project_id !== source.project_id && project.access.is_owner && !project.access.archived && !project.access.ended && project.modules.includes('tasks'));
  const { body, foot, cleanup } = openWindow({ title: t('task_transfer'), subtitle: `${task.task_key} · ${task.title}`, icon: 'branch', width: 760 });
  body.innerHTML = `<tf-select data-transfer-destination label="${escapeAttr(t('task_transfer_destination'))}" value=""><option value="">—</option>${destinations.map((project) => `<option value="${escapeAttr(project.project_id)}">${escapeHtml([...projectAncestors(project, state.projects, state.breadcrumbs).map((row) => row.name), project.name].join(' / '))}</option>`).join('')}</tf-select><div data-transfer-preview></div><div class="ps-field-hint">${escapeHtml(t('task_transfer_alias_hint'))}</div><div data-form-error class="ps-form-error" hidden></div>`;
  foot.innerHTML = `<div></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" icon="branch" data-action="transfer" disabled>${escapeHtml(t('task_transfer'))}</tf-button></div>`;
  let version = 0;
  let preview = null;
  const ready = () => !!preview && preview.destination_type_valid && preview.blocking_reasons.length === 0 && (!preview.widens_access || body.querySelector('[data-confirm-wider]')?.checked === true);
  const sync = () => foot.querySelector('[data-action="transfer"]').toggleAttribute('disabled', !ready());
  body.querySelector('[data-transfer-destination]').addEventListener('change', async () => {
    const generation = ++version; preview = null; sync();
    const destinationProjectId = body.querySelector('[data-transfer-destination]').value;
    const host = body.querySelector('[data-transfer-preview]'); host.innerHTML = '';
    if (!destinationProjectId) return;
    try {
      const result = await ApiBinary.one('projectStudioTaskTransferPreviewRequest', { sourceProjectId: source.project_id, destinationProjectId, taskId: task.task_id });
      if (!body.isConnected || generation !== version) return;
      preview = result;
      host.innerHTML = `<h3>${escapeHtml(t('task_transfer_preview'))}</h3><p>${escapeHtml(t('task_transfer_closure', { count: result.task_ids.length }))}</p><div class="ps-function-choices">${result.old_keys.map((key) => `<tf-chip status="info">${escapeHtml(key)}</tf-chip>`).join(' ')}</div>${!result.destination_type_valid ? `<div class="ps-form-error">${escapeHtml(t('task_transfer_invalid_type'))}</div>` : ''}${result.blocking_reasons.length ? `<div class="ps-form-error">${escapeHtml(t('task_transfer_blocked'))}</div><ul>${result.blocking_reasons.map((reason) => `<li>${escapeHtml(t(`task_transfer_reason_${reason}`))}</li>`).join('')}</ul>` : ''}${result.widens_access ? `<div class="ps-banner-warn">${escapeHtml(t('task_transfer_wider_hint'))}</div><tf-checkbox data-confirm-wider label="${escapeAttr(t('task_transfer_confirm_wider'))}"></tf-checkbox>` : ''}`;
      host.querySelector('[data-confirm-wider]')?.addEventListener('change', sync); sync();
    } catch (error) { const el = body.querySelector('[data-form-error]'); el.hidden = false; el.textContent = error.message; }
  });
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]'); if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    if (!ready()) return;
    button.setAttribute('disabled', '');
    try {
      const result = await ApiBinary.one('projectStudioTaskTransferRequest', { sourceProjectId: source.project_id, destinationProjectId: body.querySelector('[data-transfer-destination]').value, taskId: task.task_id, confirmWiderAccess: body.querySelector('[data-confirm-wider]')?.checked === true });
      toast(t('task_transfer_success', { key: result.new_key }), 'success'); cleanup();
      if (onSaved) await onSaved(result);
      else await openProject(result.destination_project_id, { tab: 'tasks', taskId: result.task_id });
    } catch (error) { const el = body.querySelector('[data-form-error]'); el.hidden = false; el.textContent = error.message; sync(); }
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
    // The creator becomes owner and permanent project administrator server-side.
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
        <tf-input id="ps-wz-prefix" label="${escapeAttr(t('project_key_prefix'))}" hint="${escapeAttr(t('project_key_prefix_hint'))}" maxlength="8"></tf-input>
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

      </div>
      <div class="ps-candidate-list" data-candidates hidden></div>
      <div data-team-host></div>
      <div class="ps-field-hint">${escapeHtml(t('invite_functions_hint'))}</div>
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
      <div class="ps-member-person">
        <div class="ps-av-mini">${escapeHtml(initials(state.me?.username))}</div>
        <div class="ps-member-main">
          <div class="ps-member-name">${escapeHtml(state.me?.username || '')} <tf-chip status="accent">${escapeHtml(t('you_chip'))}</tf-chip></div>
        </div>
        <tf-chip status="accent">${escapeHtml(t('access_owner'))}</tf-chip>
      </div>
    `;
    const rows = wz.members.map((member, index) => `
      <div class="ps-wizard-access" data-team-member="${index}">
        <div class="ps-members-toolbar">
          <div class="ps-member-main"><b>${escapeHtml(member.displayName)}</b><div class="ps-member-mail">${escapeHtml(member.email || '')}</div></div>
          <tf-button variant="ghost" size="sm" icon="trash" data-team-remove="${index}" title="${escapeAttr(t('wizard_member_remove'))}"></tf-button>
        </div>
        ${memberAccessFields(member, BUILTIN_FUNCTIONS, String(index))}
      </div>`).join('');
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
    const keyPrefix = body.querySelector('#ps-wz-prefix').value.trim().toUpperCase();
    if (keyPrefix && !/^[A-Z][A-Z0-9]{1,7}$/.test(keyPrefix)) { showError(t('err_project_key_prefix')); return; }
    try {
      const resp = await ApiBinary.one('projectStudioProjectCreateRequest', {
        name,
        description,
        keyPrefix,
        template: wz.template,
        modules: [...wz.modules],
        parentId: null, isPrivate: false, inheritModules: false, inheritTaskTypes: false,
        members: wz.members.map((member, index) => ({ userId: member.userId, ...readMemberAccess(body.querySelector(`[data-team-member="${index}"]`)) })),
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

  let prefixEdited = false;
  body.querySelector('#ps-wz-prefix').addEventListener('input', () => { prefixEdited = true; });
  body.querySelector('#ps-wz-name').addEventListener('input', () => {
    if (!prefixEdited) body.querySelector('#ps-wz-prefix').value = projectKeySuggestion(body.querySelector('#ps-wz-name').value);
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
        wz.members.push({
          userId,
          displayName: user.display_name ?? user.displayName ?? '',
          email: user.email || '',
          functions: ['tester'],
          projectAdmin: false,
          expiresAt: null,
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

  body.addEventListener('change', (event) => {
    const row = event.target.closest('[data-team-member]');
    if (!row) return;
    const member = wz.members[Number(row.dataset.teamMember)];
    if (!member) return;
    member.functions = [...row.querySelectorAll('[data-member-function]')].filter((item) => item.checked).map((item) => item.dataset.memberFunction);
    member.projectAdmin = row.querySelector('[data-project-admin]')?.checked === true;
    const local = row.querySelector('[data-member-expiry]')?.value || '';
    const date = local ? new Date(local) : null;
    member.expiresAt = date && Number.isFinite(date.getTime()) ? date.toISOString() : null;
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
  if (deep?.taskId || deep?.taskKey) {
    try {
      const target = await ApiBinary.one('projectStudioTaskKeyResolveRequest', { taskKey: deep.taskKey || null, taskId: deep.taskId || null, originProjectId: deep.eventId == null ? null : projectId, originEventId: deep.eventId == null ? null : String(deep.eventId) });
      projectId = target.project_id;
      deep = { ...deep, taskId: target.task_id, taskKey: null, eventId: target.event_id, tab: 'tasks' };
    } catch (error) { toast(`${t('task_load_failed')}: ${error.message}`, 'error'); return; }
  }
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
  try { state.functions = await loadCatalogue(projectId); }
  catch (error) { state.project = null; toast(`${t('members_failed')}: ${error.message}`, 'error'); return; }
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
  state.tasksView.mode = deep?.tasksMode || readTasksViewMode(projectId);
  state.tasksView.scope = readTaskScope(currentUserId(), projectId, state.tasksView.mode, descendantsOf(projectId, false).length > 0);
  state.connections = null;

  const listView = byId('ps-list-view');
  const projectView = byId('ps-project-view');
  if (listView) listView.hidden = true;
  if (!projectView) return;
  projectView.hidden = false;

  if (deep?.tab && projectTabs(projectAccess()).includes(deep.tab)) {
    state.tab = deep.tab;
  }
  if (deep?.sub && state.tab === 'tests' && TESTS_SEGMENTS.includes(deep.sub)) {
    state.f2.view = deep.sub;
  }
  renderProjectShell();
  await switchTab(state.tab);
  if (deep?.runId && state.tab === 'tests') await openRunByType(deep.runId, deep.runType);
  else if (deep?.genId && state.tab === 'tests') await openGenDetail(deep.genId);
  else if (deep?.taskId && state.tab === 'tasks') await openTaskWindow({ taskId: deep.taskId, eventId: deep.eventId, commentId: deep.commentId });

}

function updateProjectRoute(fields = {}) {
  if (!Router.current()) return;
  Router.replaceParams({ ...Router.currentParams(), projectId: projectId(), tab: state.tab, taskKey: null, eventId: null, commentId: null, ...fields });
}

function closeProject() {
  state.localAttachments.clear();
  stopAllJobTracking();
  stopChatStream();
  stopArchiveJob();
  state.project = null;
  state.connections = null;
  if (Router.current()) Router.replaceParams({ ...Router.currentParams(), projectId: null, tab: null, taskKey: null, taskId: null, eventId: null, commentId: null });
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
      try { state.functions = await loadCatalogue(projectId); }
  catch (error) { state.project = null; toast(`${t('members_failed')}: ${error.message}`, 'error'); return; }
      if (!projectTabs(projectAccess()).includes(state.tab)) state.tab = 'overview';
      renderProjectShell();
      renderTabsValue();
      await switchTab(state.tab);
    }
  } catch {
    // Header refresh is cosmetic; a failed fetch keeps the current view.
  }
}

function enabledTabs() {
  const icons = { overview: 'chart-line', knowledge: 'database', tests: 'list', tasks: 'check', chat: 'message', connections: 'brain', members: 'users', settings: 'settings' };
  return projectTabs(projectAccess()).map((id) => ({ id, icon: icons[id] }));
}

// Header subtitle carries the project context the mockup shows: what the
// operator is looking at plus the creation/last-change stamps.
function projectHeaderSubtitle(project) {
  const parts = [];
  const desc = (project.description || '').trim();
  if (desc) parts.push(desc);
  const created = fv(project, 'created_at');
  if (created) parts.push(t('head_created', { date: formatDate(created) }));
  const updated = fv(project, 'updated_at');
  if (updated) parts.push(t('head_updated', { date: formatDate(updated) }));
  return parts.join(' · ');
}

function renderProjectShell() {
  const host = byId('ps-project-view');
  const project = state.project;
  if (!host || !project) return;
  const archived = project.status === 'archived';
  const memberCount = project.member_count ?? project.memberCount ?? 0;
  const sourceCount = project.source_count ?? project.sourceCount ?? 0;
  const tabs = enabledTabs();
  const ancestors = projectAncestors(project, state.projects, state.breadcrumbs);

  host.innerHTML = `
    <tf-breadcrumb class="ps-project-crumbs" id="ps-crumbs">
      <tf-breadcrumb-item href="#">${escapeHtml(t('title'))}</tf-breadcrumb-item>
      ${ancestors.map((row) => `<tf-breadcrumb-item ${state.projects.some((item) => item.project_id === row.project_id) ? `href="#ancestor:${escapeAttr(row.project_id)}"` : ''}>${escapeHtml(row.name)}</tf-breadcrumb-item>`).join('')}
      <tf-breadcrumb-item href="#project">${escapeHtml(project.name)}</tf-breadcrumb-item>
      <tf-breadcrumb-item current>${escapeHtml(t(`tab_${state.tab}`))}</tf-breadcrumb-item>
    </tf-breadcrumb>

    <tf-detail-header title="${escapeAttr(project.name)}" subtitle="${escapeAttr(projectHeaderSubtitle(project))}" icon="folder">
      <span slot="title-actions"><tf-button variant="ghost" icon="chevron-down" data-project-picker title="${escapeAttr(t('tree_picker'))}"></tf-button></span>
      <span slot="status">
        <tf-chip status="${archived || project.lifecycle === 'ended' ? 'warn' : 'ok'}" dot>${escapeHtml(t(archived ? 'status_archived' : project.lifecycle === 'ended' ? 'status_ended' : 'status_active'))}</tf-chip>
      </span>
      <span slot="badges" class="ps-head-badges">
        <tf-chip status="accent">${sprite('users')} ${escapeHtml(t('head_members', { count: memberCount }))}</tf-chip>
        <tf-chip>${escapeHtml(accessSummary(project))}</tf-chip>
        ${functionChips(projectAccess().functions)}
        <tf-chip status="info">${sprite('database')} ${escapeHtml(t('head_sources', { count: sourceCount }))}</tf-chip>
        <tf-chip status="info">${sprite('grid-rows')} ${escapeHtml(t('modules_chip'))}: ${escapeHtml(orientationLabel(project))}</tf-chip>
      </span>
      <span slot="actions">
        ${bellHtml()}
        ${canProject('create_child') ? `<tf-button variant="ghost" icon="plus" data-new-child>${escapeHtml(t('subproject_new'))}</tf-button>` : ''}
        ${canProject('end') || canProject('resume') ? `<tf-button variant="ghost" icon="clock" data-lifecycle>${escapeHtml(t(project.lifecycle === 'ended' ? 'project_resume' : 'project_end'))}</tf-button>` : ''}
        ${canCreateTask(projectAccess()) ? `<tf-button variant="primary" icon="plus" data-new-task>${escapeHtml(t('tasks_new'))}</tf-button>` : ''}
        ${canProject('export') ? `<tf-button variant="ghost" icon="download" data-export>${escapeHtml(t('export_btn'))}</tf-button>` : ''}
        <tf-button variant="ghost" icon="users" data-goto-members ${state.tab === 'members' ? 'aria-current="page"' : ''}>${escapeHtml(t('tab_members'))}</tf-button>
        ${canArea('settings') ? `<tf-button variant="ghost" icon="settings" data-goto-settings ${state.tab === 'settings' ? 'aria-current="page"' : ''}>${escapeHtml(t('tab_settings'))}</tf-button>` : ''}
      </span>
    </tf-detail-header>

    <tf-tabs variant="underline" value="${escapeAttr(state.tab)}" id="ps-project-tabs" class="ps-project-tabs">
      ${tabs.map((tab) => {
        const c = tabCount(tab.id);
        return `<tf-tab id="${tab.id}" icon="${tab.icon}"${c ? ` count="${escapeAttr(String(c))}"` : ''}>${escapeHtml(t(`tab_${tab.id}`))}</tf-tab>`;
      }).join('')}
    </tf-tabs>

    <div id="ps-tab-panel"></div>
  `;

  // tf-breadcrumb re-renders items as anchors inside its own <nav>, so the
  // click handler must live on the container and match the rendered link.
  host.querySelector('#ps-crumbs')?.addEventListener('click', (e) => {
    const link = e.target.closest('a.tf-breadcrumb-item');
    if (!link) return;
    e.preventDefault();
    const href = link.getAttribute('href');
    if (href === '#project') selectTab('overview');
    else if (href.startsWith('#ancestor:')) switchProject(href.slice(10));
    else closeProject();
  });
  host.querySelector('[data-project-picker]').addEventListener('click', openProjectPicker);
  host.querySelector('[data-new-child]')?.addEventListener('click', () => openSubprojectWindow(project));
  host.querySelector('[data-lifecycle]')?.addEventListener('click', () => openProjectLifecycleWindow(project));
  host.querySelector('[data-new-task]')?.addEventListener('click', () => openTaskWindow({}));
  host.querySelector('[data-export]')?.addEventListener('click', () => openExportWindow());
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

// The last crumb mirrors the open tab, so the trail stays truthful after a
// tab switch (the shell itself is not re-rendered).
function syncBreadcrumbTab() {
  const crumbs = byId('ps-crumbs');
  if (!crumbs) return;
  const items = crumbs.querySelectorAll('tf-breadcrumb-item, .tf-breadcrumb-item');
  const last = items[items.length - 1];
  if (last) last.textContent = t(`tab_${state.tab}`);
}

function selectTab(tab) {
  if (state.tab === tab) return;
  switchTab(tab);
  renderTabsValue();
}

async function switchTab(tab) {
  if (!projectTabs(projectAccess()).includes(tab)) tab = 'overview';
  state.tab = tab;
  updateProjectRoute();
  renderTabsValue();
  syncBreadcrumbTab();
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
    case 'connections': await renderConnections(); break;
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

// Tab counters mirror the mockup ("Wiedza 4", "Testy 128"). Sources and members
// come with the project row; the rest arrives with the overview KPIs, so the
// counters are refreshed once those load.
function tabCount(tabId) {
  const k = state.kpis || {};
  const num = (snake, camel) => Number(k[snake] ?? k[camel] ?? 0);
  switch (tabId) {
    case 'knowledge': return Number(state.project?.source_count ?? state.project?.sourceCount ?? 0);
    case 'tests': return num('cases_total', 'casesTotal');
    case 'tasks': return num('tasks_open', 'tasksOpen');
    case 'connections': return num('ml_links', 'mlLinks');
    case 'members': return Number(state.project?.member_count ?? state.project?.memberCount ?? 0);
    default: return 0;
  }
}

function updateTabCounts() {
  const host = byId('ps-project-tabs');
  if (!host) return;
  host.querySelectorAll('tf-tab').forEach((el) => {
    const c = tabCount(el.id);
    if (c) el.setAttribute('count', String(c));
    else el.removeAttribute('count');
  });
}

async function renderOverview() {
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  let kpis = null;
  try {
    const resp = await ApiBinary.one('projectStudioOverviewRequest', { projectId: projectId() });
    kpis = resp.kpis;
    state.kpis = kpis;
    updateTabCounts();
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
  if (canCreateTask(projectAccess())) quickActions.push({ id: 'new-task', icon: 'plus', name: t('tasks_new'), sub: t('qa_new_task_sub') });
  if (canArea('knowledge', 'write') || canArea('repos', 'write')) {
    quickActions.push({ id: 'add-source', icon: 'database', name: t('qa_add_source'), sub: t('qa_add_source_sub') });
  }
  if (canArea('knowledge')) {
    quickActions.push({ id: 'kb-search', icon: 'search', name: t('qa_search'), sub: t('qa_search_sub') });
  }
  if (canArea('tests')) {
    quickActions.push({ id: 'tests', icon: 'list', name: t('qa_tests'), sub: t('qa_tests_sub') });
  }
  if (canArea('tasks')) {
    quickActions.push({ id: 'tasks', icon: 'check', name: t('qa_tasks'), sub: t('qa_tasks_sub') });
  }
  if (canArea('tasks') || canArea('board') || canArea('tests')) quickActions.push({ id: 'storage', icon: 'database', name: t('attachment_usage_title'), sub: t('attachment_usage_hint') });
  if (canArea('chat')) {
    quickActions.push({ id: 'chat', icon: 'message', name: t('qa_chat'), sub: t('qa_chat_sub') });
  }
  quickActions.push({ id: 'members', icon: 'users', name: t('qa_members'), sub: t('qa_members_sub') });

  const kv = (snake, camel) => Number(kpis?.[snake] ?? kpis?.[camel] ?? 0);
  const hasTests = canArea('tests');

  // The dashboard answers four questions — what do we have, how good is it,
  // what is broken, is the knowledge ready. Every other counter lives next to
  // its own tab or section; a wall of raw numbers here reads as noise.
  const casesTotal = kv('cases_total', 'casesTotal');
  const casesApproved = kv('cases_approved', 'casesApproved');
  const suitesTotal = kv('suites_total', 'suitesTotal');
  const tasksOpen = kv('tasks_open', 'tasksOpen');
  const defectsOpen = kv('defects_open', 'defectsOpen');

  const casesCard = hasTests ? `
      <tf-stat-card icon="list" label="${escapeAttr(t('kpi_cases'))}" value="${casesTotal}"
        delta="${escapeAttr(`${t('kpi_cases_approved_suffix', { count: casesApproved })} · ${t('kpi_suites', { count: suitesTotal })}`)}"
        delta-type="neutral"></tf-stat-card>` : '';
  const passCard = hasTests ? `
      <tf-stat-card id="ps-kpi-pass" icon="check" label="${escapeAttr(t('kpi_pass_rate'))}" value="—"></tf-stat-card>` : '';
  const defectsCard = canArea('tasks') ? `
      <tf-stat-card id="ps-kpi-defects" icon="alert" label="${escapeAttr(t('kpi_defects'))}" value="${defectsOpen}"
        accent="${defectsOpen > 0 ? 'danger' : 'success'}"
        delta="${escapeAttr(t('kpi_tasks_open_suffix', { count: tasksOpen }))}"
        delta-type="${defectsOpen > 0 ? 'warn' : 'neutral'}"></tf-stat-card>` : '';
  const knowledgeCard = canArea('knowledge') ? `
      <tf-stat-card icon="database" label="${escapeAttr(t('kpi_sources'))}" value="${sourcesReady}/${sourcesTotal}"
        delta="${escapeAttr(openJobs > 0 ? t('kpi_open_jobs', { count: openJobs }) : t('kpi_sources_ready_suffix', { count: sourcesReady }))}"
        delta-type="${openJobs > 0 ? 'warn' : 'neutral'}"></tf-stat-card>` : '';

  // Charts only make sense for a project that runs tests.
  const chartsRow = hasTests ? `
    <div class="ps-overview-charts">
      <tf-section-card title="${escapeAttr(t('chart_pass_trend'))}" icon="trend">
        <div id="ps-chart-trend" class="ps-chart-host"></div>
      </tf-section-card>
      <tf-section-card title="${escapeAttr(t('chart_last_runs'))}" icon="bar-chart">
        <span slot="subtitle">${escapeHtml(t('chart_last_runs_legend'))}</span>
        <div id="ps-chart-runs" class="ps-chart-host"></div>
      </tf-section-card>
    </div>` : '';

  panel.innerHTML = `
    <div class="ps-kpi-grid ps-kpi-overview">
      ${casesCard}
      ${passCard}
      ${defectsCard}
      ${knowledgeCard}
    </div>

    ${chartsRow}

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
            <tf-button variant="ghost" wrap class="ps-quick-action" data-qa="${escapeAttr(qa.id)}">
              <div class="ps-qa-ico">${sprite(qa.icon)}</div>
              <div>
                <div class="ps-qa-name">${escapeHtml(qa.name)}</div>
                <div class="ps-qa-sub">${escapeHtml(qa.sub)}</div>
              </div>
            </tf-button>
          `).join('')}
        </div>
        ${sourcesReady > 0 && hasTests && canArea('tests', 'write') ? `
          <div class="ps-qa-banner" id="ps-qa-generate">
            <div class="ps-qa-banner-head">${sprite('sparkle')}<span>${escapeHtml(t('qa_generate_title'))}</span></div>
            <div class="ps-qa-banner-body">${escapeHtml(t('qa_generate_body'))}
              <a href="#" data-qa-generate>${escapeHtml(t('qa_generate_link'))}</a>
            </div>
          </div>` : ''}
      </tf-section-card>
    </div>
  `;

  if (hasTests) loadOverviewCharts();
  renderActivityFeed();
  byId('ps-activity-more-btn')?.addEventListener('click', () => loadMoreActivity());
  panel.querySelectorAll('[data-qa]').forEach((el) => {
    el.addEventListener('click', () => {
      const id = el.dataset.qa;
      if (id === 'add-source') { selectTab('knowledge'); setTimeout(() => openSourceWindow(null), 0); }
      else if (id === 'kb-search') { state.kbView = 'search'; selectTab('knowledge'); }
      else if (id === 'tests') selectTab('tests');
      else if (id === 'tasks') selectTab('tasks');
      else if (id === 'new-task') openTaskWindow({});
      else if (id === 'chat') selectTab('chat');
      else if (id === 'members') selectTab('members');
      else if (id === 'storage') openAttachmentUsageWindow();
    });
  });
  panel.querySelector('[data-qa-generate]')?.addEventListener('click', (e) => {
    e.preventDefault();
    f2().view = 'generations';
    selectTab('tests');
    setTimeout(() => openGenerationWindow(), 0);
  });
}

// Pass-rate and the two charts are derived from finished runs rather than from
// OverviewKpis: the counters carry totals, while the dashboard needs the trend
// and the last-run comparison. Failures here leave the cards in their empty
// state — an overview must still render when reports are unavailable.
async function loadOverviewCharts() {
  const pid = projectId();
  const decided = (r) => Number(r.passed ?? 0) + Number(r.failed ?? 0) + Number(r.blocked ?? 0);
  const rate = (r) => (decided(r) ? (Number(r.passed ?? 0) / decided(r)) * 100 : null);

  let runs = [];
  try {
    const resp = await ApiBinary.one('projectStudioRunsListRequest', {
      projectId: pid, status: 'completed', runType: '', offset: 0, limit: 8,
    });
    runs = (Array.isArray(resp.runs) ? resp.runs : []).slice().reverse();
  } catch { /* keep the empty state */ }
  if (state.tab !== 'overview' || projectId() !== pid) return;

  // KPI: last finished run + delta against the previous one.
  const card = byId('ps-kpi-pass');
  if (card && runs.length) {
    const last = runs[runs.length - 1];
    const prev = runs.length > 1 ? runs[runs.length - 2] : null;
    const lastRate = rate(last);
    if (lastRate !== null) {
      card.setAttribute('value', lastRate.toFixed(1).replace('.', ','));
      card.setAttribute('suffix', '%');
      const prevRate = prev ? rate(prev) : null;
      if (prevRate !== null) {
        const diff = lastRate - prevRate;
        card.setAttribute('delta', t('kpi_pass_delta', { pts: `${diff >= 0 ? '+' : '−'}${Math.abs(diff).toFixed(1).replace('.', ',')}` }));
        card.setAttribute('delta-type', diff >= 0 ? 'up' : 'down');
      } else {
        card.setAttribute('delta', t('kpi_pass_first'));
        card.setAttribute('delta-type', 'neutral');
      }
    }
  }

  // Bar chart: the last 8 finished runs, passed/failed/blocked stacked.
  const runsHost = byId('ps-chart-runs');
  if (runsHost) {
    if (!runs.length) runsHost.innerHTML = `<div class="ps-chart-empty">${escapeHtml(t('chart_no_runs'))}</div>`;
    else {
      runsHost.innerHTML = '<tf-bar-chart></tf-bar-chart>';
      const chart = runsHost.firstElementChild;
      const xs = runs.map((r) => `#${r.run_no ?? r.runNo ?? ''}`);
      const serie = (id, key, tone, name) => ({
        id, name, tone, showInLegend: true,
        points: runs.map((r, i) => ({ x: xs[i], y: Number(r[key] ?? 0) })),
      });
      chart.height = 150;
      chart.stacking = 'stacked';
      chart.legend = { position: 'none' };
      chart.series = [
        serie('passed', 'passed', 'success', t('run_passed')),
        serie('failed', 'failed', 'critical', t('run_failed')),
        serie('blocked', 'blocked', 'warning', t('run_blocked')),
      ];
    }
  }

  // Line chart: 30-day pass-rate trend from the reports engine.
  const trendHost = byId('ps-chart-trend');
  if (!trendHost) return;
  const to = new Date();
  const from = new Date(to.getTime() - 29 * 86400000);
  const iso = (d) => d.toISOString().slice(0, 10);
  let rows = [];
  try {
    const resp = await ApiBinary.one('projectStudioReportQueryRequest', {
      projectId: pid, report: 'runs_over_time', fromDate: iso(from), toDate: iso(to), suiteId: '', runIds: [],
    });
    rows = JSON.parse(resp.rows_json ?? resp.rowsJson ?? '[]');
  } catch { /* keep the empty state */ }
  if (state.tab !== 'overview' || projectId() !== pid) return;
  if (!Array.isArray(rows) || !rows.length) {
    trendHost.innerHTML = `<div class="ps-chart-empty">${escapeHtml(t('chart_no_trend'))}</div>`;
    return;
  }
  const pts = rows.map((r) => {
    const total = ['passed', 'failed', 'blocked', 'skipped'].reduce((a, k) => a + Number(rowVal(r, [k], 0)), 0);
    const passed = Number(rowVal(r, ['passed'], 0));
    return { x: String(rowVal(r, ['date'], '')).slice(5), y: total ? (passed / total) * 100 : 0 };
  });
  trendHost.innerHTML = '<tf-line-chart></tf-line-chart>';
  const line = trendHost.firstElementChild;
  line.height = 150;
  line.legend = { position: 'none' };
  // Dates are labels, not numbers — without a category scale the axis renders a
  // 0..1 numeric range and the points land off-chart.
  line.xAxis = { scale: 'category', min: null, max: null, ticks: null, format: null };
  line.yAxis = { scale: 'linear', min: 0, max: 100, ticks: 4, format: (v) => `${v}%` };
  line.series = [{
    id: 'pass', name: t('chart_pass_trend_series'), tone: 'success', style: 'solid', showInLegend: true,
    points: pts.map((p) => ({ x: p.x, y: Number(p.y.toFixed(1)) })),
  }];
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
        <option value="sources" icon="catalog">${escapeHtml(t('kb_view_sources'))}</option>
        <option value="search" icon="search">${escapeHtml(t('kb_view_search'))}</option>
        <option value="files" icon="file-text">${escapeHtml(t('kb_view_files'))}</option>
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

function canSourceWrite(source) {
  return !!source && canArea(['git', 'zip'].includes(source.kind) ? 'repos' : 'knowledge', 'write');
}

function canRefreshRepo() {
  return canArea('repos', 'write') && canArea('settings', 'write');
}

function canFileWrite(file) {
  const sourceId = fv(file, 'source_id') || state.files.sourceId;
  return canSourceWrite(state.sources.find((source) => fv(source, 'source_id') === sourceId));
}

function sourceRowHtml(source) {
  const sourceId = source.source_id ?? source.sourceId;
  const status = source.status;
  const job = source.last_job ?? source.lastJob;
  const jobId = job ? (job.job_id ?? job.jobId) : null;
  const running = job && job.status === 'running';
  const fileCount = source.file_count ?? source.fileCount ?? 0;
  const chunkCount = source.chunk_count ?? source.chunkCount ?? 0;
  const mutable = canSourceWrite(source);

  let config = {};
  try { config = JSON.parse(source.config_json ?? source.configJson ?? '{}') || {}; } catch { config = {}; }
  const meta = [
    t(`kind_${source.kind}`),
    source.kind === 'git' && config.branch ? t('source_git_branch_meta', { branch: config.branch }) : '',
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
    // Git sources fetch + delta re-index ("Odśwież"); everything else re-runs
    // the full ingest.
    if (source.kind === 'git' && canRefreshRepo()) {
      actions.push(`<tf-button variant="ghost" size="sm" icon="refresh" data-refresh="${escapeAttr(sourceId)}">${escapeHtml(t('source_refresh'))}</tf-button>`);
    } else {
      actions.push(`<tf-button variant="ghost" size="sm" icon="refresh" data-reingest="${escapeAttr(sourceId)}" title="${escapeAttr(t('action_reingest'))}"></tf-button>`);
    }
    if (source.kind === 'api_spec') {
      actions.push(`<tf-button variant="ghost" size="sm" icon="code" data-endpoints="${escapeAttr(sourceId)}" title="${escapeAttr(t('source_endpoints'))}"></tf-button>`);
    }
    actions.push(`
      <tf-button variant="ghost" size="sm" icon="chevron-down" data-source-more title="${escapeAttr(t('action_more'))}"></tf-button>
      <tf-menu placement="bottom-end" data-source-menu>
        <tf-menu-item action="edit" icon="edit">${escapeHtml(t('action_edit'))}</tf-menu-item>
        ${source.kind === 'git' && canRefreshRepo() ? `<tf-menu-item action="refresh" icon="refresh">${escapeHtml(t('source_refresh'))}</tf-menu-item>` : ''}
        ${source.kind === 'api_spec' ? `<tf-menu-item action="endpoints" icon="code">${escapeHtml(t('source_endpoints'))}</tf-menu-item>` : ''}
        <tf-menu-item action="reingest" icon="refresh">${escapeHtml(t('action_reingest'))}</tf-menu-item>
        <tf-menu-item action="files" icon="file-text">${escapeHtml(t('action_show_files'))}</tf-menu-item>
        <tf-menu-divider></tf-menu-divider>
        <tf-menu-item action="delete" icon="trash" danger>${escapeHtml(t('action_delete_source'))}</tf-menu-item>
      </tf-menu>
    `);
  } else {
    if (source.kind === 'api_spec') {
      actions.push(`<tf-button variant="ghost" size="sm" icon="code" data-endpoints="${escapeAttr(sourceId)}" title="${escapeAttr(t('source_endpoints'))}"></tf-button>`);
    }
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
        ${canArea('knowledge', 'write') || canArea('repos', 'write') ? `<tf-button variant="primary" size="sm" icon="plus" id="ps-add-source">${escapeHtml(t('add_source'))}</tf-button>` : ''}
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
    const refresh = e.target.closest('[data-refresh]');
    if (refresh) { refreshGitSource(refresh.dataset.refresh); return; }
    const endpoints = e.target.closest('[data-endpoints]');
    if (endpoints) { openApiSpecEndpoints(endpoints.dataset.endpoints); return; }
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
      case 'refresh': refreshGitSource(sourceId); break;
      case 'endpoints': openApiSpecEndpoints(sourceId); break;
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

// W01 "Odśwież" — git fetch + delta re-index; the returned job is tracked like
// any other ingest job.
async function refreshGitSource(sourceId) {
  try {
    const resp = await ApiBinary.one('projectStudioSourceRefreshRequest', { projectId: projectId(), sourceId });
    toast(t('source_refresh_started'), 'success');
    const jobId = fv(resp, 'job_id');
    await renderSources();
    if (jobId) trackIngestJob(jobId);
  } catch (err) {
    toast(`${t('source_refresh_failed')}: ${err.message}`, 'error');
  }
}

// W02 — endpoint list parsed out of an OpenAPI/Swagger source.
async function openApiSpecEndpoints(sourceId) {
  const source = state.sources.find((s) => fv(s, 'source_id') === sourceId);
  let endpoints = [];
  try {
    const resp = await ApiBinary.one('projectStudioApiSpecEndpointsRequest', { projectId: projectId(), sourceId });
    const parsed = JSON.parse(fv(resp, 'endpoints_json') || '[]');
    endpoints = Array.isArray(parsed) ? parsed : [];
  } catch (err) {
    toast(`${t('source_endpoints_failed')}: ${err.message}`, 'error');
    return;
  }
  const { body, foot, cleanup } = openWindow({
    title: t('source_endpoints_title'),
    subtitle: source?.name || '',
    icon: 'code',
    width: 900,
  });
  body.innerHTML = endpoints.length
    ? `<div id="ps-endpoints-host"></div>`
    : `<tf-empty-state icon="code" title="${escapeAttr(t('source_endpoints_empty'))}"></tf-empty-state>`;
  foot.innerHTML = `
    <div class="ps-footer-left"><span class="ps-field-hint">${escapeHtml(t('source_endpoints_count', { count: endpoints.length }))}</span></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" icon="download" data-action="export">${escapeHtml(t('reports_export_csv'))}</tf-button>
      <tf-button variant="ghost" data-action="close-endpoints">${escapeHtml(t('action_close'))}</tf-button>
    </div>
  `;
  if (endpoints.length) {
    const hostEl = body.querySelector('#ps-endpoints-host');
    hostEl.innerHTML = `
      <tf-table id="ps-endpoints-table">
        <tf-column key="method" label="${escapeAttr(t('endpoints_col_method'))}"></tf-column>
        <tf-column key="path" label="${escapeAttr(t('endpoints_col_path'))}"></tf-column>
        <tf-column key="summary" label="${escapeAttr(t('endpoints_col_summary'))}"></tf-column>
        <tf-column key="tags" label="${escapeAttr(t('endpoints_col_tags'))}"></tf-column>
      </tf-table>
    `;
    hostEl.querySelector('#ps-endpoints-table').rows = endpoints.map((ep) => ({
      method: String(ep.method || ''),
      path: String(ep.path || ''),
      summary: String(ep.summary || ''),
      tags: (Array.isArray(ep.tags) ? ep.tags : []).join(', '),
    }));
  }
  foot.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'export') {
      downloadTextFile('endpoints.csv', toCsv(endpoints.map((ep) => ({
        method: ep.method,
        path: ep.path,
        summary: ep.summary,
        operation_id: fv(ep, 'operation_id') || '',
        tags: (Array.isArray(ep.tags) ? ep.tags : []).join(' '),
      }))));
      return;
    }
    cleanup();
  });
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
    kind: canArea('knowledge', 'write') ? 'document' : 'git',
    // [{ file, progress (0-100 | null), ref }]
    files: [],
    busy: false,
  };

  const kindsHtml = SOURCE_KINDS.filter((kind) => canArea(['git', 'zip'].includes(kind.id) ? 'repos' : 'knowledge', 'write')).map((k) => `
    <div class="ps-choice-card ${sw.kind === k.id ? 'is-selected' : ''}"
         data-kind="${escapeAttr(k.id)}" role="button" tabindex="0">
      <div class="ps-cc-ico">${sprite(k.icon)}</div>
      <div>
        <div class="ps-cc-name">${escapeHtml(t(`kind_${k.id}`))}</div>
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
    <div data-kind-form="git" hidden>
      <tf-input id="ps-src-git-url" label="${escapeAttr(t('source_git_url_label'))}" placeholder="https://github.com/org/repo.git"
        hint="${escapeAttr(t('source_git_url_hint'))}"></tf-input>
      <div class="ps-git-grid">
        <tf-input id="ps-src-git-branch" label="${escapeAttr(t('source_git_branch_label'))}" value="main"
          hint="${escapeAttr(t('source_git_branch_hint'))}"></tf-input>
        <tf-input id="ps-src-git-subdir" label="${escapeAttr(t('source_git_subdir_label'))}" placeholder="src/"
          hint="${escapeAttr(t('source_git_subdir_hint'))}"></tf-input>
      </div>
      <tf-input id="ps-src-git-token" type="password" label="${escapeAttr(t('source_git_token_label'))}"
        hint="${escapeAttr(t('source_git_token_hint'))}" ${canRefreshRepo() ? '' : 'disabled'}></tf-input>
      <div class="ps-field-hint">${escapeHtml(t('source_git_limits_hint'))}</div>
    </div>
    <div data-kind-form="zip" hidden>
      <div class="ps-field">
        <span class="ps-field-label">${escapeHtml(t('source_zip_label'))}</span>
        <tf-file-input id="ps-src-zip" accept=".zip" label="${escapeAttr(t('source_zip_dropzone'))}"></tf-file-input>
        <div class="ps-field-hint">${escapeHtml(t('source_zip_hint'))}</div>
      </div>
      <div data-zip-file></div>
    </div>
    <div data-kind-form="api_spec" hidden>
      <div class="ps-field">
        <span class="ps-field-label">${escapeHtml(t('source_spec_label'))}</span>
        <tf-file-input id="ps-src-spec" accept=".yaml,.yml,.json" label="${escapeAttr(t('source_spec_dropzone'))}"></tf-file-input>
        <div class="ps-field-hint">${escapeHtml(t('source_spec_hint'))}</div>
      </div>
      <div data-spec-file></div>
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

  body.querySelectorAll('[data-kind-form]').forEach((form) => { form.hidden = form.dataset.kindForm !== sw.kind; });
  body.querySelector('[data-kind-grid]')?.addEventListener('click', (e) => {
    const card = e.target.closest('[data-kind]');
    if (!card || sw.busy) return;
    sw.kind = card.dataset.kind;
    // Every kind owns its own upload slot, so the queue never leaks across a
    // switch (a document queue must not become the ZIP archive).
    sw.files = [];
    body.querySelectorAll('[data-file-list], [data-zip-file], [data-spec-file]').forEach((slot) => {
      slot.innerHTML = '';
    });
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

  // ZIP archives and OpenAPI specs are single-file sources: the newest pick
  // replaces the previous one instead of queueing.
  const singleFilePicked = (selector, file) => {
    sw.files = file ? [{ file, progress: null, ref: null }] : [];
    const slot = body.querySelector(selector);
    if (!slot) return;
    slot.innerHTML = file ? `
      <div class="ps-added-file">
        <span class="ps-af-ico">${sprite('folder')}</span>
        <div class="ps-af-main">
          <div class="ps-af-name">${escapeHtml(file.name)}</div>
          <div class="ps-af-size">${escapeHtml(formatBytes(file.size))}</div>
        </div>
      </div>
    ` : '';
  };
  body.querySelector('#ps-src-zip')?.addEventListener('change', (e) => {
    singleFilePicked('[data-zip-file]', e.detail?.files?.[0] ?? null);
  });
  body.querySelector('#ps-src-spec')?.addEventListener('change', (e) => {
    singleFilePicked('[data-spec-file]', e.detail?.files?.[0] ?? null);
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
    } else if (sw.kind === 'git') {
      const repoUrl = String(body.querySelector('#ps-src-git-url')?.value ?? '').trim();
      if (!/^https?:\/\/.+/.test(repoUrl)) { showError(t('err_source_git_url')); return; }
      const branch = String(body.querySelector('#ps-src-git-branch')?.value ?? '').trim() || 'main';
      const subdir = String(body.querySelector('#ps-src-git-subdir')?.value ?? '').trim();
      const token = String(body.querySelector('#ps-src-git-token')?.value ?? '');
      // The token is stripped server-side into the encrypted column; it never
      // stays in config_json (which every source listing reads back).
      configJson = JSON.stringify({ repo_url: repoUrl, branch, subdir, ...(token ? { token } : {}) });
    } else if (sw.kind === 'zip') {
      if (!sw.files.length) { showError(t('err_source_zip')); return; }
    } else if (sw.kind === 'api_spec') {
      if (!sw.files.length) { showError(t('err_source_spec')); return; }
    }

    showError(null);
    sw.busy = true;
    btn.setAttribute('disabled', '');
    try {
      const fileRefs = [];
      if (sw.kind === 'document' || sw.kind === 'zip' || sw.kind === 'api_spec') {
        for (let i = 0; i < sw.files.length; i += 1) {
          sw.files[i].progress = 0;
          if (sw.kind === 'document') renderFileList();
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
    ${source.kind === 'git' ? `
      <tf-input id="ps-src-edit-git-url" label="${escapeAttr(t('source_git_url_label'))}" value="${escapeAttr(config.repo_url || '')}"
        hint="${escapeAttr(t('source_git_url_hint'))}"></tf-input>
      <div class="ps-git-grid">
        <tf-input id="ps-src-edit-branch" label="${escapeAttr(t('source_git_branch_label'))}" value="${escapeAttr(config.branch || 'main')}"></tf-input>
        <tf-input id="ps-src-edit-subdir" label="${escapeAttr(t('source_git_subdir_label'))}" value="${escapeAttr(config.subdir || '')}"></tf-input>
      </div>
      ${canRefreshRepo() ? `<tf-input id="ps-src-edit-token" type="password" label="${escapeAttr(t('source_git_token_label'))}"
        hint="${escapeAttr(t('source_token_update_hint'))}"></tf-input>
      <div class="ps-token-actions">
        <tf-button variant="ghost" size="sm" icon="key" id="ps-src-token-save">${escapeHtml(t('source_token_save'))}</tf-button>
        <tf-button variant="ghost" size="sm" icon="trash" id="ps-src-token-clear">${escapeHtml(t('source_token_clear'))}</tf-button>
      </div>` : ''}
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

  // The git token is set/cleared through its own input-only request; it never
  // travels back in config_json.
  const setGitToken = async (token) => {
    try {
      await ApiBinary.one('projectStudioSourceSecretSetRequest', { projectId: projectId(), sourceId, token });
      toast(t('source_token_saved'), 'success');
      const input = body.querySelector('#ps-src-edit-token');
      if (input) input.value = '';
    } catch (err) {
      toast(`${t('source_token_failed')}: ${err.message}`, 'error');
    }
  };
  body.querySelector('#ps-src-token-save')?.addEventListener('click', () => {
    const token = String(body.querySelector('#ps-src-edit-token')?.value ?? '');
    if (!token) { toast(t('source_token_empty'), 'error'); return; }
    setGitToken(token);
  });
  body.querySelector('#ps-src-token-clear')?.addEventListener('click', () => setGitToken(null));

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
    if (source.kind === 'git') {
      const repoUrl = String(body.querySelector('#ps-src-edit-git-url')?.value ?? '').trim();
      if (!/^https?:\/\/.+/.test(repoUrl)) {
        if (errEl) { errEl.hidden = false; errEl.textContent = t('err_source_git_url'); }
        return;
      }
      configJson = JSON.stringify({
        ...config,
        repo_url: repoUrl,
        branch: String(body.querySelector('#ps-src-edit-branch')?.value ?? '').trim() || 'main',
        subdir: String(body.querySelector('#ps-src-edit-subdir')?.value ?? '').trim(),
      });
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
  // tf-searchbox swallows Enter and emits its own "search" event, so the keydown
  // handler alone never fired the query — pressing Enter looked like nothing
  // happened at all.
  byId('ps-kb-query')?.addEventListener('search', (e) => {
    state.kbQuery = String(e.detail?.value ?? '');
    runKbSearch();
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
  state.kbError = '';
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
    state.kbError = err.message;
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
  if (state.kbError) {
    host.innerHTML = `<tf-empty-state icon="warning" title="${escapeAttr(t('kb_search_failed'))}" message="${escapeAttr(state.kbError)}"></tf-empty-state>`;
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
      <span slot="actions">
        <tf-segmented id="ps-files-mode" value="${escapeAttr(state.files.mode)}">
          <option value="tree" icon="folder">${escapeHtml(t('files_mode_tree'))}</option>
          <option value="table" icon="list">${escapeHtml(t('files_mode_table'))}</option>
        </tf-segmented>
      </span>
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

  byId('ps-files-mode')?.addEventListener('change', (e) => {
    state.files.mode = e.detail?.value === 'table' ? 'table' : 'tree';
    renderFilesTable();
  });

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

// Files arrive as a flat list of paths; the tree view (mockup W04) folds them
// into directories so a repository source is navigable.
function buildFileTree(rows) {
  const root = { children: new Map(), files: [] };
  for (const f of rows) {
    const parts = String(f.path || '').split('/').filter(Boolean);
    const name = parts.pop() || String(f.path || '');
    let node = root;
    for (const dir of parts) {
      if (!node.children.has(dir)) node.children.set(dir, { children: new Map(), files: [] });
      node = node.children.get(dir);
    }
    node.files.push({ name, file: f });
  }
  const toNodes = (node, prefix) => {
    const dirs = [...node.children.entries()]
      .sort((a, b) => a[0].localeCompare(b[0]))
      .map(([name, child]) => ({
        id: `dir:${prefix}${name}`,
        label: name,
        icon: 'folder',
        children: toNodes(child, `${prefix}${name}/`),
      }));
    const files = node.files
      .sort((a, b) => a.name.localeCompare(b.name))
      .map(({ name, file }) => ({
        id: `file:${file.file_id ?? file.fileId}`,
        label: name,
        icon: 'file-text',
      }));
    return [...dirs, ...files];
  };
  return toNodes(root, '');
}

function renderFilesTree(host) {
  host.innerHTML = `
    <div class="ps-files-split">
      <div class="ps-files-tree"><tf-tree id="ps-files-tree" selectable></tf-tree></div>
      <div class="ps-files-preview" id="ps-files-preview">
        <div class="ps-field-hint">${escapeHtml(t('files_pick_hint'))}</div>
      </div>
    </div>
  `;
  const tree = byId('ps-files-tree');
  tree.nodes = buildFileTree(state.files.rows);
  tree.addEventListener('select', (e) => {
    const id = String(e.detail?.id || '');
    if (!id.startsWith('file:')) return;
    renderFilePreviewPane(id.slice(5));
  });
}

async function renderFilePreviewPane(fileId) {
  const pane = byId('ps-files-preview');
  if (!pane) return;
  const file = state.files.rows.find((f) => (f.file_id ?? f.fileId) === fileId);
  if (!file) return;
  pane.innerHTML = `<div class="ps-loading"><tf-spinner size="sm"></tf-spinner> ${escapeHtml(t('loading'))}</div>`;
  let content = '';
  let truncated = false;
  try {
    const resp = await ApiBinary.one('projectStudioSourceFilePreviewRequest', {
      projectId: projectId(), fileId, maxBytes: 256 * 1024,
    });
    content = String(resp.content ?? '');
    truncated = !!resp.truncated;
  } catch (err) {
    pane.innerHTML = `<div class="ps-form-error">${escapeHtml(err.message)}</div>`;
    return;
  }
  if (byId('ps-files-preview') !== pane) return;

  pane.innerHTML = `
    <div class="ps-files-preview-head">
      <div>
        <div class="ps-files-preview-path">${escapeHtml(file.path)}</div>
        <div class="ps-files-preview-meta">
          ${escapeHtml(t('files_chunk_count', { count: file.chunk_count ?? file.chunkCount ?? 0 }))}
          · ${escapeHtml(formatBytes(Number(file.size_bytes ?? file.sizeBytes ?? 0)))}
          · ${escapeHtml(formatTimestamp(file.updated_at ?? file.updatedAt))}
        </div>
      </div>
      <tf-chip status="${FILE_STATUS_CHIP[file.status] || 'info'}">${escapeHtml(t(`file_status_${file.status}`))}</tf-chip>
      <span class="ps-toolbar-spacer"></span>
      <tf-button variant="ghost" size="sm" icon="copy" id="ps-files-copy-path">${escapeHtml(t('files_copy_path'))}</tf-button>
      ${canFileWrite(file) ? `<tf-button variant="ghost" size="sm" icon="trash" id="ps-files-remove">${escapeHtml(t('files_remove_from_index'))}</tf-button>` : ''}
    </div>
    ${truncated ? `<div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('preview_truncated'))}</span></div>` : ''}
    <tf-code-editor id="ps-files-code" language="${escapeAttr(codeLanguageFor(file.path))}" readonly></tf-code-editor>
  `;
  const editor = byId('ps-files-code');
  if (editor) editor.value = content;
  byId('ps-files-copy-path')?.addEventListener('click', () => {
    navigator.clipboard?.writeText(file.path);
    toast(t('files_path_copied'), 'success');
  });
  byId('ps-files-remove')?.addEventListener('click', () => confirmDeleteFile({ _id: fileId, path: file.path }));
}

function codeLanguageFor(path) {
  const ext = String(path || '').split('.').pop().toLowerCase();
  return {
    js: 'javascript', mjs: 'javascript', ts: 'typescript', tsx: 'typescript',
    py: 'python', rs: 'rust', json: 'json', yaml: 'yaml', yml: 'yaml',
    md: 'markdown', html: 'html', css: 'css', sql: 'sql', sh: 'bash',
  }[ext] || 'text';
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
  if (state.files.mode === 'tree') {
    renderFilesTree(tableHost);
    updateFilesPager();
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
    _file: f,
    path: f.path,
    status: { status: FILE_STATUS_CHIP[f.status] || 'info', label: t(`file_status_${f.status}`) },
    // Skip/error reason surfaces inline (skipped files carry it in `error`).
    note: f.error || '—',
    size: formatBytes(Number(f.size_bytes ?? f.sizeBytes ?? 0)),
    chunks: f.chunk_count ?? f.chunkCount ?? 0,
    updated: formatTimestamp(f.updated_at ?? f.updatedAt),
  }));
  table.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const wrap = document.createElement('div');
    wrap.className = 'ps-file-actions';
    const previewBtn = document.createElement('tf-button');
    previewBtn.setAttribute('variant', 'ghost');
    previewBtn.setAttribute('size', 'sm');
    previewBtn.setAttribute('icon', 'eye');
    previewBtn.setAttribute('title', t('kb_preview'));
    previewBtn.addEventListener('click', (e) => { e.stopPropagation(); openFilePreview(live()._id); });
    wrap.appendChild(previewBtn);
    if (canFileWrite(row._file)) {
      const delBtn = document.createElement('tf-button');
      delBtn.setAttribute('variant', 'ghost');
      delBtn.setAttribute('size', 'sm');
      delBtn.setAttribute('icon', 'trash');
      delBtn.setAttribute('title', t('action_delete'));
      delBtn.addEventListener('click', (e) => { e.stopPropagation(); confirmDeleteFile(live()); });
      wrap.appendChild(delBtn);
    }
    return wrap;
  };
  table.addEventListener('row-click', (e) => {
    const id = e.detail?.row?._id;
    if (id) openFilePreview(id);
  });

  updateFilesPager();
}

function updateFilesPager() {
  const pager = byId('ps-files-pager');
  if (!pager) return;
  pager.hidden = state.files.total <= FILES_PAGE_SIZE;
  const from = state.files.offset + 1;
  const to = Math.min(state.files.offset + state.files.rows.length, state.files.total);
  const range = byId('ps-files-range');
  if (range) range.textContent = t('files_range', { from, to, total: state.files.total });
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
        <tf-button variant="primary" size="sm" icon="plus" id="ps-chat-new" ${canSendChat() ? '' : 'hidden'}>${escapeHtml(t('chat_new'))}</tf-button>
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

  sizeChatLayout();

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
        <div class="ps-conv-actions" ${canArea('chat', 'write') ? '' : 'hidden'}>
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
  if (renameBtn) renameBtn.hidden = !chat || !canArea('chat', 'write');
  if (deleteBtn) deleteBtn.hidden = !chat || !canArea('chat', 'write');
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
    if (state.chatBusy || !canSendChat()) composer.setAttribute('disabled', '');
    else composer.removeAttribute('disabled');
  }
}

function nowTime() {
  return new Date().toLocaleTimeString(I18n.getLanguage(), { hour: '2-digit', minute: '2-digit' });
}

async function sendChatMessage(text) {
  if (state.chatBusy || !canSendChat()) return;

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
    const [response, functions] = await Promise.all([
      ApiBinary.one('projectStudioMembersListRequest', { projectId: projectId() }),
      loadCatalogue(projectId()),
    ]);
    state.members = Array.isArray(response.members) ? response.members : [];
    state.functions = functions;
    state.memberAgents = null;
    if (canArea('settings')) {
      const settingsResponse = await ApiBinary.one('projectStudioSettingsGetRequest', { projectId: projectId() });
      state.memberAgents = Array.isArray(settingsResponse.settings?.agents) ? settingsResponse.settings.agents : [];
    }
  } catch (error) {
    panel.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('members_failed')}: ${error.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'members') return;
  panel.innerHTML = `
    <div class="ps-access-heading">
      <div><h2>${escapeHtml(t('functions_title'))}</h2><div class="ps-field-hint">${escapeHtml(t('access_no_functions_hint'))}</div></div>
      ${canManageMembers() ? `<tf-button variant="primary" icon="plus" id="ps-invite">${escapeHtml(t('members_invite'))}</tf-button>` : ''}
    </div>
    <div class="ps-members-toolbar">
      <tf-searchbox id="ps-members-search" placeholder="${escapeAttr(t('members_search_placeholder'))}" debounce="200"></tf-searchbox>
      <tf-select id="ps-members-function-filter" value="${escapeAttr(state.memberFunctionFilter)}">
        <option value="all">${escapeHtml(t('functions_filter_all'))}</option>
        ${state.functions.map((definition) => `<option value="${escapeAttr(fv(definition, 'function_id'))}">${escapeHtml(functionLabel(definition))}</option>`).join('')}
      </tf-select>
      <span class="ps-toolbar-spacer"></span>
      <span class="ps-field-hint">${escapeHtml(t('members_enabled_modules'))}: ${escapeHtml((fv(projectAccess(), 'enabled_modules') || []).map((module) => t(`module_${module}`)).join(', '))}</span>
    </div>
    <div id="ps-members-list"></div>
    ${state.memberAgents !== null ? `<tf-section-card title="${escapeAttr(t('members_agents'))}" icon="brain">
      <span slot="subtitle">${escapeHtml(t('members_agents_hint'))}</span>
      <div class="ps-agent-bindings">${state.memberAgents.filter((binding) => fv(binding, 'agent_id')).map((binding) => `
        <div class="ps-agent-binding"><tf-chip status="accent">${escapeHtml(t(`agents_fn_${binding.function}`))}</tf-chip>
          <span>${escapeHtml(fv(binding, 'agent_name') || fv(binding, 'agent_id'))}</span>
          <span class="ps-field-hint">${escapeHtml(fv(binding, 'model_label') || '')}</span></div>`).join('') || `<div class="ps-field-hint">${escapeHtml(t('members_agents_empty'))}</div>`}</div>
    </tf-section-card>` : ''}
    <tf-section-card title="${escapeAttr(t('function_catalogue'))}" icon="shield">
      <span slot="actions">${canManageMembers() ? `<tf-button variant="ghost" size="sm" icon="plus" id="ps-function-new">${escapeHtml(t('function_new'))}</tf-button>` : ''}</span>
      <div class="ps-function-catalogue" id="ps-function-catalogue"></div>
      <details class="ps-access-matrix">
        <summary>${escapeHtml(t('access_matrix'))}</summary>
        <div class="ps-field-hint">${escapeHtml(t('access_matrix_hint'))}</div>
        <div class="ps-permission-key">${PERMISSION_LEVELS.map((level) => `<tf-chip status="${permissionChipStatus(level)}">${permissionLetter(level)} · ${escapeHtml(t(`access_level_${level}`))}</tf-chip>`).join('')}</div>
        <div class="ps-matrix-scroll"><tf-table id="ps-permission-matrix">
          <tf-column key="area" label="${escapeAttr(t('access_area'))}" renderer="html"></tf-column>
          ${state.functions.map((definition, index) => `<tf-column key="function${index}" label="${escapeAttr(functionLabel(definition))}" renderer="html"></tf-column>`).join('')}
        </tf-table></div>
      </details>
    </tf-section-card>`;
  byId('ps-members-search').value = state.memberQuery;
  byId('ps-members-search')?.addEventListener('search', (event) => {
    state.memberQuery = String(event.detail?.value || '');
    renderMembersList();
  });
  byId('ps-members-function-filter')?.addEventListener('change', (event) => {
    state.memberFunctionFilter = event.detail?.value || 'all';
    renderMembersList();
  });
  byId('ps-invite')?.addEventListener('click', openInviteWindow);
  byId('ps-function-new')?.addEventListener('click', () => openFunctionWindow());
  renderMembersList();
  renderFunctionCatalogue();
}

function permissionChipStatus(level) {
  return ({ none: 'info', read: 'info', write: 'ok', admin: 'accent' })[level] || 'info';
}

function permissionLetter(level) {
  return ({ none: '—', read: 'R', write: 'W', admin: 'A' })[level] || '—';
}

function areaLabel(area) {
  return t(`access_area_${area.replace('.', '_')}`);
}

function effectiveAccessHtml(access) {
  const grants = (access?.areas || []).filter((grant) => grant.enabled && grant.level !== 'none');
  const create = canCreateTask(access) ? `<tf-chip status="accent">${escapeHtml(t('tasks_new'))}</tf-chip>` : '';
  return grants.length || create ? `<div class="ps-effective-access">${grants.map((grant) => `<tf-chip status="${permissionChipStatus(grant.level)}" title="${escapeAttr(t(`access_level_${grant.level}`))}">${escapeHtml(areaLabel(grant.area))} ${permissionLetter(grant.level)}</tf-chip>`).join(' ')} ${create}</div>`
    : `<span class="ps-field-hint">${escapeHtml(t('access_level_none'))}</span>`;
}

function renderMembersList() {
  const host = byId('ps-members-list');
  if (!host) return;
  const query = state.memberQuery.trim().toLowerCase();
  const visible = state.members.filter((member) =>
    (state.memberFunctionFilter === 'all' || member.functions?.includes(state.memberFunctionFilter))
    && (!query || `${fv(member, 'display_name')} ${member.email || ''}`.toLowerCase().includes(query)));
  const groups = ['people', 'inherited', 'temporary', 'expired'];
  host.innerHTML = groups.map((group) => {
    const rows = visible.filter((member) => memberGroup(member) === group);
    return `<tf-section-card title="${escapeAttr(t(`members_${group}`))}" icon="${group === 'people' ? 'users' : 'clock'}">
      <span slot="subtitle">${rows.length}</span>
      ${rows.length ? `<tf-table data-member-table="${group}" actions-label="${escapeAttr(t('action_more'))}">
        <tf-column key="person" label="${escapeAttr(t('members_people'))}" renderer="html"></tf-column>
        <tf-column key="functions" label="${escapeAttr(t('functions_label'))}" renderer="html"></tf-column>
        <tf-column key="access" label="${escapeAttr(t('access_summary'))}" renderer="html"></tf-column>
        <tf-column key="expiry" label="${escapeAttr(t('members_expiry'))}" renderer="html"></tf-column>
      </tf-table>` : `<div class="ps-field-hint">${escapeHtml(t('members_empty'))}</div>`}
    </tf-section-card>`;
  }).join('') + `<div class="ps-table-footer">${escapeHtml(t('members_access_footer', {
    shown: visible.length, total: state.members.length,
    admins: state.members.filter((member) => member.active && fv(member, 'project_admin')).length,
    temporary: state.members.filter((member) => memberGroup(member) === 'temporary').length,
  }))}</div>`;
  for (const group of groups) {
    const table = host.querySelector(`[data-member-table="${group}"]`);
    if (!table) continue;
    table.rowKey = '_id';
    table.rows = visible.filter((member) => memberGroup(member) === group).map((member) => {
      const userId = fv(member, 'user_id');
      const name = fv(member, 'display_name') || userId;
      return {
        _id: userId, _member: member,
        person: `<div class="ps-member-person"><div class="ps-av-mini">${escapeHtml(initials(name))}</div><div><b>${escapeHtml(name)}</b>
          ${isMe(userId) ? `<tf-chip status="accent">${escapeHtml(t('you_chip'))}</tf-chip>` : ''}
          <div class="ps-member-mail">${escapeHtml(member.email || '')}</div>
          <div class="ps-member-markers">${fv(member, 'is_owner') ? `<tf-chip status="accent">${escapeHtml(t('access_owner'))}</tf-chip>` : ''}
          ${fv(member, 'project_admin') ? `<tf-chip status="accent">${escapeHtml(t('access_project_admin'))}</tf-chip>` : ''}</div>${member.origin_projects?.some((row) => row.project_id !== projectId()) ? `<div class="ps-field-hint">${escapeHtml(t('members_origin', { projects: member.origin_projects.filter((row) => row.project_id !== projectId()).map((row) => row.name).join(', ') }))}</div>` : ''}</div></div>`,
        functions: `<div class="ps-member-function-chips">${functionChips(member.functions)}</div>`,
        access: effectiveAccessHtml(member.access),
        expiry: `${escapeHtml(fv(member, 'expires_at') ? formatTimestamp(fv(member, 'expires_at')) : t('members_permanent'))}
          ${member.active ? '' : `<tf-chip status="warn">${escapeHtml(t('members_expired_badge'))}</tf-chip>`}`,
      };
    });
    if (canManageMembers() || canTransferOwnership()) table.rowActions = (row) => {
      const button = document.createElement('tf-button');
      button.setAttribute('variant', 'ghost');
      button.setAttribute('size', 'sm');
      button.setAttribute('icon', 'more');
      button.setAttribute('title', t('action_more'));
      button.addEventListener('click', () => {
        const member = row._member;
        const owner = !!fv(member, 'is_owner');
        const self = isMe(row._id);
        const items = [];
        if (canManageMembers()) items.push({ label: t(member.inherited ? 'members_add_local' : 'access_edit'), icon: member.inherited ? 'plus' : 'edit', run: () => openMemberAccessWindow(member) });
        if (canTransferOwnership() && !member.inherited && !owner && !self) items.push({ label: t('members_transfer'), icon: 'key', disabled: !member.active, reason: t('members_expired_badge'), run: () => transferOwnership(row._id) });
        if (canManageMembers() && !member.inherited) items.push({ label: t('members_handover'), icon: 'arrow', danger: true, disabled: owner || self,
          reason: t(owner ? 'members_owner_handover' : 'members_self_remove'),
          run: () => openHandover({ userId: row._id, reason: 'project_removal', projectId: projectId() }) });
        openActionMenu(button, items, fv(member, 'display_name') || row._id);
      });
      return button;
    };
  }
}

function renderFunctionCatalogue() {
  const host = byId('ps-function-catalogue');
  if (!host) return;
  host.innerHTML = state.functions.map((definition, index) => `<div class="ps-function-row">
    <div><b>${escapeHtml(functionLabel(definition))}</b><div class="ps-field-hint">${escapeHtml(functionDescription(definition))}</div></div>
    ${definition.builtin ? `<tf-chip status="info">${escapeHtml(t('function_builtin'))}</tf-chip>` : ''}
    ${canManageMembers() ? `<tf-button variant="ghost" size="sm" icon="more" data-function-menu="${index}" title="${escapeAttr(t('action_more'))}"></tf-button>` : ''}
  </div>`).join('');
  host.querySelectorAll('[data-function-menu]').forEach((button) => button.addEventListener('click', () => {
    const definition = state.functions[Number(button.dataset.functionMenu)];
    const items = [{ label: t('function_edit'), icon: 'edit', run: () => openFunctionWindow(definition) }];
    if (!definition.builtin) items.push({ label: t('function_delete'), icon: 'trash', danger: true, run: () => {
      openConfirmWindow({ kind: 'delete', subject: { name: functionLabel(definition), icon: 'shield' }, anchor: button,
        title: t('function_delete'), submitLabel: t('action_delete'),
        consequence: t('function_delete_message', { name: functionLabel(definition) }),
        onSubmit: async () => {
          const response = await ApiBinary.one('projectStudioFunctionDeleteRequest', { projectId: projectId(), functionId: fv(definition, 'function_id') });
          if (!response.ok) throw new Error(t('members_failed'));
          toast(t('function_delete_ok'), 'success');
          await refreshProjectHeader();
        },
      });
    } });
    openActionMenu(button, items, functionLabel(definition));
  }));
  const matrix = byId('ps-permission-matrix');
  matrix.style.minWidth = `${Math.max(800, state.functions.length * 145 + 180)}px`;
  matrix.rows = PROJECT_AREAS.map((area) => {
    const enabled = projectAccess().areas?.find((grant) => grant.area === area)?.enabled;
    const row = { area: `<b>${escapeHtml(areaLabel(area))}</b>${enabled ? '' : `<div class="ps-field-hint">${escapeHtml(t('access_module_disabled'))}</div>`}` };
    state.functions.forEach((definition, index) => {
      const level = definition.grants.find((grant) => grant.area === area)?.level || 'none';
      row[`function${index}`] = `<tf-chip status="${permissionChipStatus(level)}" title="${escapeAttr(t(`access_level_${level}`))}">${permissionLetter(level)} · ${escapeHtml(t(`access_level_${level}`))}</tf-chip>`;
    });
    return row;
  });
}

function openFunctionWindow(definition = null) {
  if (!canManageMembers()) return;
  const { body, foot, cleanup } = openWindow({ title: t(definition ? 'function_edit' : 'function_new'), icon: 'shield', width: 720 });
  body.innerHTML = `
    <div class="ps-access-catalogue-fields">
      <tf-input data-function-id label="${escapeAttr(t('function_id'))}" value="${escapeAttr(fv(definition, 'function_id') || '')}" ${definition ? 'disabled' : ''}></tf-input>
      <tf-input data-function-name label="${escapeAttr(t('function_name'))}" value="${escapeAttr(definition ? functionLabel(definition) : '')}" maxlength="128"></tf-input>
      <tf-textarea data-function-description label="${escapeAttr(t('function_description'))}" rows="3"></tf-textarea>
    </div>
    <div class="ps-function-grants">${PROJECT_AREAS.map((area) => {
      const level = definition?.grants.find((grant) => grant.area === area)?.level || 'none';
      return `<tf-select data-function-area="${escapeAttr(area)}" label="${escapeAttr(areaLabel(area))}" value="${level}">
        ${PERMISSION_LEVELS.map((value) => `<option value="${value}" ${value === level ? 'selected' : ''}>${escapeHtml(t(`access_level_${value}`))}</option>`).join('')}</tf-select>`;
    }).join('')}</div>
    <div class="ps-form-error" data-form-error hidden></div>`;
  body.querySelectorAll('[data-function-area]').forEach((select) => {
    select.setOptions(PERMISSION_LEVELS.map((value) => ({ value, label: t(`access_level_${value}`) })), definition?.grants.find((grant) => grant.area === select.dataset.functionArea)?.level || 'none');
  });
  body.querySelector('[data-function-description]').value = definition ? functionDescription(definition) : '';
  foot.innerHTML = `<div class="ps-footer-left"></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" icon="check" data-action="save">${escapeHtml(t('action_save'))}</tf-button></div>`;
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]');
    if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    button.setAttribute('disabled', '');
    try {
      const response = await ApiBinary.one('projectStudioFunctionSaveRequest', {
        projectId: projectId(), function: {
          functionId: String(body.querySelector('[data-function-id]').value || '').trim(),
          name: definition && String(body.querySelector('[data-function-name]').value || '').trim() === functionLabel(definition) ? definition.name : String(body.querySelector('[data-function-name]').value || '').trim(),
          description: definition && String(body.querySelector('[data-function-description]').value || '').trim() === functionDescription(definition) ? definition.description : String(body.querySelector('[data-function-description]').value || '').trim(),
          builtin: definition?.builtin === true,
          grants: [...body.querySelectorAll('[data-function-area]')].map((select) => ({ area: select.dataset.functionArea, level: select.value })),
        },
      });
      if (!response.ok) throw new Error(t('members_failed'));
      cleanup();
      toast(t('function_save_ok'), 'success');
      await refreshProjectHeader();
    } catch (error) {
      const message = body.querySelector('[data-form-error]');
      message.hidden = false;
      message.textContent = error.message;
      button.removeAttribute('disabled');
    }
  });
}

function openMemberAccessWindow(member) {
  if (!canManageMembers()) return;
  const { body, foot, cleanup } = openWindow({ title: t(member.inherited ? 'members_add_local' : 'access_edit'), subtitle: fv(member, 'display_name') || fv(member, 'user_id'), icon: 'users', width: 640 });
  body.innerHTML = `${memberAccessFields(member, state.functions)}${member.origin_projects?.some((row) => row.project_id !== projectId()) ? `<div class="ps-field-hint">${escapeHtml(t('members_local_removal_hint'))}</div>` : ''}<div class="ps-field-hint">${escapeHtml(t('access_no_functions_hint'))}</div><div class="ps-form-error" data-form-error hidden></div>`;
  foot.innerHTML = `<div class="ps-footer-left"></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" icon="check" data-action="save">${escapeHtml(t('action_save'))}</tf-button></div>`;
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]');
    if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    try {
      const access = readMemberAccess(body);
      button.setAttribute('disabled', '');
      const response = member.inherited
        ? await ApiBinary.one('projectStudioMembersAddRequest', { projectId: projectId(), members: [{ userId: fv(member, 'user_id'), ...access }] })
        : await ApiBinary.one('projectStudioMemberAccessSetRequest', { projectId: projectId(), userId: fv(member, 'user_id'), ...access });
      if (member.inherited ? response.added !== 1 : !response.ok) throw new Error(t('members_failed'));
      cleanup();
      toast(t('access_saved'), 'success');
      await refreshProjectHeader();
    } catch (error) {
      const message = body.querySelector('[data-form-error]');
      message.hidden = false;
      message.textContent = error.message;
      button.removeAttribute('disabled');
    }
  });
}

async function transferOwnership(userId) {
  const member = state.members.find((entry) => fv(entry, 'user_id') === userId);
  if (!member || !canTransferOwnership()) return;
  const ok = await TfWindow.confirm({ title: t('members_transfer_title'),
    message: t('members_transfer_message', { name: escapeHtml(fv(member, 'display_name') || '') }),
    confirmLabel: t('members_transfer'), cancelLabel: t('action_cancel'), danger: true });
  if (!ok) return;
  try {
    await ApiBinary.one('projectStudioOwnershipTransferRequest', { projectId: projectId(), newOwnerUserId: userId });
    toast(t('members_transfer_ok'), 'success');
    await refreshProjectHeader();
  } catch (error) { toast(`${t('members_transfer_failed')}: ${error.message}`, 'error'); }
}

function openInviteWindow() {
  if (!canManageMembers()) return;
  const { body, foot, cleanup } = openWindow({ title: t('invite_title'), icon: 'users', width: 640 });
  const invite = { selected: [], candidates: [] };
  body.innerHTML = `
    <tf-searchbox data-invite-search placeholder="${escapeAttr(t('wizard_member_search'))}" debounce="250"></tf-searchbox>
    <div class="ps-selected-chips" data-invite-chips></div>
    <div class="ps-candidate-list" data-invite-candidates hidden></div>
    ${memberAccessFields({ functions: ['tester'], projectAdmin: false, expiresAt: null }, state.functions)}
    <div class="ps-field-hint">${escapeHtml(t('invite_functions_hint'))}</div>
    <div class="ps-form-error" data-form-error hidden></div>`;
  foot.innerHTML = `<div class="ps-footer-left"></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" icon="plus" data-action="save">${escapeHtml(t('invite_submit'))}</tf-button></div>`;
  const renderSelected = () => {
    const host = body.querySelector('[data-invite-chips]');
    host.innerHTML = invite.selected.map((person, index) => `<tf-chip status="accent" removable data-invite-chip="${index}">${escapeHtml(fv(person, 'display_name') || fv(person, 'user_id'))}</tf-chip>`).join('');
    host.querySelectorAll('[data-invite-chip]').forEach((chip) => chip.addEventListener('remove', () => {
      invite.selected.splice(Number(chip.dataset.inviteChip), 1);
      renderSelected();
      renderCandidates();
    }));
  };
  const renderCandidates = () => {
    const host = body.querySelector('[data-invite-candidates]');
    const chosen = new Set(invite.selected.map((person) => fv(person, 'user_id')));
    const effectiveIds = new Set(state.members.filter((member) => member.access.has_access).map((member) => member.user_id));
    const rows = invite.candidates.filter((person) => !chosen.has(fv(person, 'user_id')) && !effectiveIds.has(fv(person, 'user_id')));
    host.hidden = !rows.length;
    host.innerHTML = rows.map((person) => `<tf-button variant="ghost" data-invite-candidate="${escapeAttr(fv(person, 'user_id'))}">${escapeHtml(`${fv(person, 'display_name') || ''} · ${person.email || ''}`)}</tf-button>`).join('');
  };
  body.querySelector('[data-invite-search]').addEventListener('search', async (event) => {
    const query = String(event.detail?.value || '').trim();
    if (!query) { invite.candidates = []; renderCandidates(); return; }
    try {
      const response = await ApiBinary.one('projectStudioMemberCandidatesRequest', { projectId: projectId(), query, limit: 12 });
      invite.candidates = Array.isArray(response.users) ? response.users : [];
      renderCandidates();
    } catch (error) {
      const message = body.querySelector('[data-form-error]');
      message.hidden = false;
      message.textContent = error.message;
    }
  });
  body.addEventListener('click', (event) => {
    const candidate = event.target.closest('[data-invite-candidate]');
    if (!candidate) return;
    const person = invite.candidates.find((entry) => fv(entry, 'user_id') === candidate.dataset.inviteCandidate);
    if (person && !invite.selected.some((entry) => fv(entry, 'user_id') === fv(person, 'user_id'))) invite.selected.push(person);
    renderSelected();
    renderCandidates();
  });
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]');
    if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    try {
      if (!invite.selected.length) throw new Error(t('err_invite_empty'));
      const access = readMemberAccess(body);
      button.setAttribute('disabled', '');
      const response = await ApiBinary.one('projectStudioMembersAddRequest', { projectId: projectId(), members: invite.selected.map((person) => ({ userId: fv(person, 'user_id'), ...access })) });
      toast(t('invite_ok', { count: Number(response.added) }), 'success');
      cleanup();
      await refreshProjectHeader();
    } catch (error) {
      const message = body.querySelector('[data-form-error]');
      message.hidden = false;
      message.textContent = error.message;
      button.removeAttribute('disabled');
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
  const mutable = canArea('settings', 'write');
  const agents = Array.isArray(settings.agents) ? settings.agents : [];
  const tags = Array.isArray(settings.tags) ? settings.tags : [];
  const enabledModules = new Set(
    Array.isArray(settings.modules) ? settings.modules : (Array.isArray(state.project?.modules) ? state.project.modules : []),
  );
  const moduleLocked = (mod) => mod.locked && !state.project.parent_id && state.project.template !== 'empty';
  const graphExtraction = fv(settings, 'graph_extraction') === true;

  const bindingOf = (fn) => agents.find((a) => a.function === fn)
    || { function: fn, agent_id: '', agent_name: '', model_label: '' };
  const agentOptionsFor = (binding) => {
    const agentId = fv(binding, 'agent_id') || '';
    const options = [
      `<option value="" ${!agentId ? 'selected' : ''}>${escapeHtml(t('agents_default_option'))}</option>`,
      ...state.agentOptions.map((a) => `<option value="${escapeAttr(a.id)}" ${a.id === agentId ? 'selected' : ''}>${escapeHtml(a.name)}</option>`),
    ];
    // Keep an unknown current binding visible even when the agent list failed.
    if (agentId && !state.agentOptions.some((a) => a.id === agentId)) {
      options.push(`<option value="${escapeAttr(agentId)}" selected>${escapeHtml(fv(binding, 'agent_name') || agentId)}</option>`);
    }
    return options.join('');
  };
  const agentRowHtml = (fn) => {
    const binding = bindingOf(fn);
    const modelLabel = fv(binding, 'model_label') || '';
    return `
      <div class="ps-setting-row">
        <div class="ps-sr-main">
          <div class="ps-sr-label">${escapeHtml(t(`agents_fn_${fn}`))}</div>
          <div class="ps-sr-desc">${escapeHtml(t(`agents_fn_${fn}_desc`))}</div>
        </div>
        <div class="ps-fn-controls">
          <tf-select data-agent-fn="${escapeAttr(fn)}" value="${escapeAttr(fv(binding, 'agent_id') || '')}">${agentOptionsFor(binding)}</tf-select>
          <span class="ps-fn-model" data-agent-model="${escapeAttr(fn)}">${escapeHtml(modelLabel ? `${t('agents_model_prefix')}: ${modelLabel}` : '')}</span>
        </div>
      </div>
    `;
  };

  panel.innerHTML = `
    <tf-section-card title="${escapeAttr(t('settings_basics_title'))}" icon="settings">
      <div class="ps-field" style="margin-bottom:12px;">
        <tf-input id="ps-set-name" label="${escapeAttr(t('wizard_name_label'))}" value="${escapeAttr(settings.name ?? state.project?.name ?? '')}"
          hint="${escapeAttr(t('settings_name_hint'))}"></tf-input>
      </div>
      <div class="ps-field" style="margin-bottom:12px;">
        <tf-input id="ps-set-prefix" label="${escapeAttr(t('project_key_prefix'))}" value="${escapeAttr(fv(state.project, 'key_prefix'))}" maxlength="8" ${fv(state.project, 'key_prefix_locked') ? 'readonly' : ''}
          hint="${escapeAttr(t(fv(state.project, 'key_prefix_locked') ? 'project_key_prefix_locked' : 'project_key_prefix_hint'))}"></tf-input>
      </div>
      <div class="ps-field" style="margin-bottom:12px;">
        <tf-textarea id="ps-set-desc" label="${escapeAttr(t('wizard_desc_label'))}" rows="3"
          hint="${escapeAttr(t('settings_desc_hint'))}"></tf-textarea>
      </div>
      <tf-button variant="primary" icon="check" id="ps-set-save">${escapeHtml(t('settings_save'))}</tf-button>
    </tf-section-card>

    ${projectAccess().enabled_modules?.includes('tasks') ? `<tf-section-card title="${escapeAttr(t('task_types_title'))}" icon="list"><div id="ps-set-task-types" class="ps-task-types"></div></tf-section-card>` : ''}

    <tf-section-card title="${escapeAttr(t('settings_modules_title'))}" icon="grid-rows">
      <span slot="subtitle">${escapeHtml(t('settings_modules_hint'))}</span>
      <div id="ps-set-modules">
        ${MODULE_DEFS.map((mod) => `
          <div class="ps-module-row">
            <div class="ps-source-ico">${sprite(mod.icon)}</div>
            <div class="ps-module-main">
              <div class="ps-module-name">
                ${escapeHtml(t(`module_${mod.id}`))}
                ${moduleLocked(mod) ? `<tf-chip status="accent">${escapeHtml(t('module_required'))}</tf-chip>` : ''}
              </div>
              <div class="ps-module-desc">${escapeHtml(t(`module_${mod.id}_desc`))}</div>
            </div>
            <tf-toggle data-module="${escapeAttr(mod.id)}" ${enabledModules.has(mod.id) ? 'checked' : ''} ${moduleLocked(mod) || state.project.inherit_modules ? 'disabled' : ''}></tf-toggle>
          </div>
        `).join('')}
      </div>
      ${state.project.inherit_modules ? `<span class="ps-field-hint">${escapeHtml(t('inheritance_live'))}</span>` : ''}
      <tf-button variant="primary" icon="check" id="ps-set-modules-save" ${state.project.inherit_modules ? 'disabled' : ''}>${escapeHtml(t('settings_save'))}</tf-button>
    </tf-section-card>

    <tf-section-card title="${escapeAttr(t('settings_graph_title'))}" icon="network">
      <span slot="subtitle">${escapeHtml(t('settings_graph_sub'))}</span>
      <div class="ps-setting-row">
        <div class="ps-sr-main">
          <div class="ps-sr-label">${escapeHtml(t('settings_graph_label'))}</div>
          <div class="ps-sr-desc">${escapeHtml(t('settings_graph_desc'))}</div>
        </div>
        <tf-toggle id="ps-set-graph" ${graphExtraction ? 'checked' : ''}></tf-toggle>
      </div>
      <div class="ps-graph-cost">${sprite('alert')}<span>${escapeHtml(t('settings_graph_cost'))}</span></div>
      <tf-button variant="primary" icon="check" id="ps-set-graph-save">${escapeHtml(t('settings_save'))}</tf-button>
    </tf-section-card>

    <tf-section-card title="${escapeAttr(t('agents_title'))}" icon="brain">
      <span slot="subtitle">${escapeHtml(t('agents_sub'))}</span>
      <div id="ps-agent-rows">
        ${AGENT_FUNCTIONS.map(agentRowHtml).join('')}
      </div>
      <div class="ps-field-hint">${escapeHtml(t('agents_generators_hint'))}</div>
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

    <tf-section-card title="${escapeAttr(t('subprojects_title'))}" icon="branch"><span slot="subtitle">${escapeHtml(t('subprojects_hint'))}</span><div id="ps-set-subprojects"></div></tf-section-card>

    <div class="ps-danger-zone">
      <div class="ps-danger-title">${sprite('alert')}${escapeHtml(t('danger_title'))}</div>
      <div class="ps-danger-row">
        <div class="ps-sr-main">
          <div class="ps-sr-label">${escapeHtml(t(archived ? 'danger_unarchive_label' : 'danger_archive_label'))}</div>
          <div class="ps-sr-desc">${escapeHtml(t('danger_archive_desc'))}</div>
        </div>
        <tf-button variant="ghost" icon="clock" id="ps-danger-archive" ${canProject(archived ? 'unarchive' : 'archive') ? '' : 'disabled'}>${escapeHtml(t(archived ? 'action_unarchive' : 'action_archive'))}</tf-button>
      </div>
      <div class="ps-danger-row">
        <div class="ps-sr-main">
          <div class="ps-sr-label">${escapeHtml(t('danger_delete_label'))}</div>
          <div class="ps-sr-desc">${escapeHtml(t('danger_delete_desc'))}</div>
        </div>
        <tf-button variant="danger-solid" icon="trash" id="ps-danger-delete" ${canProject('delete') ? '' : `disabled title="${escapeAttr(t('danger_owner_only'))}"`}>${escapeHtml(t('action_delete'))}</tf-button>
      </div>
    </div>
  `;

  if (!mutable) panel.querySelectorAll('tf-input, tf-textarea, tf-select, tf-toggle, tf-button').forEach((control) => {
    if (control.id === 'ps-danger-archive' && canProject(archived ? 'unarchive' : 'archive') || control.id === 'ps-danger-delete' && canProject('delete')) return;
    control.setAttribute('disabled', '');
  });
  const descField = panel.querySelector('#ps-set-desc');
  if (descField) descField.value = settings.description ?? state.project?.description ?? '';

  byId('ps-set-save')?.addEventListener('click', async () => {
    const name = String(byId('ps-set-name')?.value ?? '').trim();
    const description = String(byId('ps-set-desc')?.value ?? '').trim();
    if (name.length < 3) {
      toast(t('err_name_short'), 'error');
      return;
    }
    const keyPrefix = fv(state.project, 'key_prefix_locked') ? null : byId('ps-set-prefix').value.trim().toUpperCase();
    if (keyPrefix !== null && !/^[A-Z][A-Z0-9]{1,7}$/.test(keyPrefix)) { toast(t('err_project_key_prefix'), 'error'); return; }
    try {
      await ApiBinary.one('projectStudioSettingsSaveRequest', { projectId: projectId(), name, description, keyPrefix });
      toast(t('settings_saved'), 'success');
      await refreshProjectHeader();
    } catch (err) {
      toast(`${t('settings_save_failed')}: ${err.message}`, 'error');
    }
  });

  renderSubprojectSettings(byId('ps-set-subprojects'));
  if (byId('ps-set-task-types')) await renderTaskTypes(byId('ps-set-task-types'));

  // Modules replace the enabled set; only the original non-empty root presets
  // require Knowledge. An own empty preset can remain a grouping node.
  byId('ps-set-modules-save')?.addEventListener('click', async () => {
    if (state.project.inherit_modules || !canArea('settings', 'write')) return;
    const host = byId('ps-set-modules');
    const modules = MODULE_DEFS
      .filter((mod) => moduleLocked(mod) || host?.querySelector(`tf-toggle[data-module="${CSS.escape(mod.id)}"]`)?.hasAttribute('checked'))
      .map((mod) => mod.id);
    try {
      await ApiBinary.one('projectStudioSettingsSaveRequest', { projectId: projectId(), modules });
      toast(t('settings_modules_saved'), 'success');
      // Modules drive the tab strip, so the shell must be rebuilt from the
      // freshly read project row rather than the stale one in state.
      await refreshProjectHeader();
    } catch (err) {
      toast(`${t('settings_save_failed')}: ${err.message}`, 'error');
    }
  });

  // The toggle is submitted ALONE: `graph_extraction` is `None` on every other
  // save, so turning it on is a deliberate act and no unrelated save can flip
  // it. Re-reads the stored value afterwards rather than trusting the switch.
  byId('ps-set-graph-save')?.addEventListener('click', async () => {
    const toggle = byId('ps-set-graph');
    const enabled = toggle?.hasAttribute('checked') === true;
    try {
      await ApiBinary.one('projectStudioSettingsSaveRequest', {
        projectId: projectId(),
        graphExtraction: enabled,
      });
      const resp = await ApiBinary.one('projectStudioSettingsGetRequest', { projectId: projectId() });
      state.settings = resp.settings;
      const stored = fv(resp.settings ?? {}, 'graph_extraction') === true;
      if (toggle) {
        if (stored) toggle.setAttribute('checked', '');
        else toggle.removeAttribute('checked');
      }
      toast(t(stored ? 'settings_graph_on' : 'settings_graph_off'), 'success');
    } catch (err) {
      toast(`${t('settings_save_failed')}: ${err.message}`, 'error');
    }
  });

  // agents_json REPLACES the whole binding map server-side, so every change
  // resubmits the full set read from the current selects.
  byId('ps-agent-rows')?.addEventListener('change', async (e) => {
    const select = e.target.closest('[data-agent-fn]');
    if (!select) return;
    const bindings = AGENT_FUNCTIONS.map((fn) => ({
      function: fn,
      agent_id: String(byId('ps-agent-rows')?.querySelector(`[data-agent-fn="${CSS.escape(fn)}"]`)?.value ?? ''),
    }));
    try {
      await ApiBinary.one('projectStudioSettingsSaveRequest', {
        projectId: projectId(),
        agentsJson: JSON.stringify(bindings),
      });
      toast(t('agents_saved'), 'success');
      // Re-fetch so the resolved model labels next to the selects stay honest.
      const resp = await ApiBinary.one('projectStudioSettingsGetRequest', { projectId: projectId() });
      state.settings = resp.settings;
      const fresh = Array.isArray(resp.settings?.agents) ? resp.settings.agents : [];
      for (const fn of AGENT_FUNCTIONS) {
        const binding = fresh.find((a) => a.function === fn);
        const modelEl = byId('ps-agent-rows')?.querySelector(`[data-agent-model="${CSS.escape(fn)}"]`);
        const modelLabel = binding ? (fv(binding, 'model_label') || '') : '';
        if (modelEl) modelEl.textContent = modelLabel ? `${t('agents_model_prefix')}: ${modelLabel}` : '';
      }
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
    if (canProject('delete')) confirmDeleteProject(state.project);
  });
}

// =============================================================================
// F2 — shared helpers (wire field access, chips, tags, members, attachments)
// =============================================================================

const F2_PAGE_SIZE = 25;
const GEN_POLL_MS = 3000;

const CASE_STATUS_CHIP = { draft: 'info', review: 'warn', approved: 'ok', deprecated: 'err' };
const PRIORITY_CHIP = { low: 'info', medium: 'accent', high: 'warn', critical: 'err' };
const RUN_STATUS_CHIP = { running: 'accent', completed: 'ok', cancelled: 'warn', error: 'err' };
const ITEM_STATUS_CHIP = {
  pending: 'info', in_progress: 'accent', running: 'accent', passed: 'ok',
  failed: 'err', blocked: 'warn', skipped: 'info', error: 'err',
};
const GEN_STATUS_CHIP = { running: 'accent', review: 'warn', accepted: 'ok', rejected: 'err', failed: 'err', cancelled: 'info' };
const TASK_STATUS_CHIP = { todo: 'info', in_progress: 'accent', review: 'warn', done: 'ok' };
const CASE_KINDS = ['manual', 'ui', 'api', 'unit', 'perf', 'security'];
const CASE_KIND_CHIP = { manual: 'neutral', ui: 'info', api: 'accent', unit: 'ok', perf: 'warn', security: 'err' };
const PRIORITIES = ['low', 'medium', 'high', 'critical'];
const TASK_STATUSES = ['todo', 'in_progress', 'review', 'done'];
const NOTIF_KIND_ICON = {
  run_item_assigned: 'play',
  run_closed: 'check',
  generation_finished: 'sparkle',
  task_assigned: 'edit',
  task_reassigned: 'users',
  task_unassigned: 'edit',
  task_status_changed: 'check',
  task_mentioned: 'message',
  task_handed_over: 'send',
  task_handed_back: 'refresh',
  environment_pending: 'shield',
  environment_decided: 'globe',
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

// The chat is a full-height workspace: the layout takes whatever vertical space
// is left under the header/tabs, so only the message list scrolls and the
// composer stays pinned to the bottom of the screen.
function sizeChatLayout() {
  const layout = document.querySelector('.ps-chat-layout');
  if (!layout) return;
  const top = layout.getBoundingClientRect().top;
  layout.style.height = `${Math.max(420, window.innerHeight - top - 24)}px`;
}

// One listener for the whole module — it no-ops whenever the chat is not open.
window.addEventListener('resize', () => sizeChatLayout());

// Date only (no clock) for header stamps.
function formatDate(value) {
  if (!value) return '';
  const raw = String(value);
  const date = new Date(raw.includes('T') ? raw : `${raw.replace(' ', 'T')}Z`);
  if (Number.isNaN(date.getTime())) return raw;
  return date.toLocaleDateString(I18n.getLanguage());
}

// UUIDs are unreadable in dense lists; the first segment is enough to tell
// two rows apart and matches how the mockups label cases (TC-118).
function shortId(id) {
  const s = String(id || '');
  return s.length > 8 ? s.slice(0, 8) : s;
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

// Only current test writers can execute an assigned run.
function testerMembers() {
  return (f2().membersCache || []).filter((member) => allowsArea(member.access, 'tests', 'write'));
}

function memberName(userId) {
  const m = (f2().membersCache || []).find((x) => fv(x, 'user_id') === userId);
  return m ? (fv(m, 'display_name') || '') : '';
}

function attachmentOwner(ownerKind, ownerId, stepIndex = null, pid = projectId()) {
  return { projectId: pid, ownerKind, ownerId, stepIndex };
}

async function uploadAttachmentFile(file, options = {}) {
  const attachment = await uploadProjectAttachment(file, { projectId: projectId(), userId: currentUserId(), ...options });
  state.localAttachments.set(`${projectId()}:${attachment.sha256}`, file);
  return attachment;
}

function attachmentError(error) {
  return ['attachment_stream_reload', 'attachment_resume_file_mismatch'].includes(error.message) ? t(error.message) : error.message;
}

async function openAttachmentPreview(attachment, owner) {
  const localFile = owner.ownerId ? null : state.localAttachments.get(`${owner.projectId}:${attachment.sha256}`);
  if (!owner.ownerId && !localFile) { toast(t('attachment_save_first'), 'info'); return; }
  const { body, foot, cleanup, signal } = openWindow({ title: attachment.name, subtitle: `${attachment.mime} · ${formatBytes(attachment.size_bytes)}`, icon: attachment.mime.startsWith('video/') ? 'play' : 'paperclip', width: 900 });
  body.innerHTML = `<div class="ps-att-preview" data-preview-media></div><div class="ps-field-hint" data-preview-status></div><tf-button variant="ghost" icon="refresh" data-preview-retry hidden>${escapeHtml(t('attachment_preview_retry'))}</tf-button><div data-download-progress></div><div class="ps-form-error" data-form-error hidden></div>`;
  foot.innerHTML = `<div></div><div class="ps-footer-right"><tf-button variant="primary" icon="download" data-action="download">${escapeHtml(t('attachment_download_original'))}</tf-button><tf-button variant="ghost" data-action="close">${escapeHtml(t('action_close'))}</tf-button></div>`;
  const media = body.querySelector('[data-preview-media]');
  const status = body.querySelector('[data-preview-status]');
  const retry = body.querySelector('[data-preview-retry]');
  const errorEl = body.querySelector('[data-form-error]');
  let stream = null;
  let localUrl = null;
  let pollTimer = null;
  let closed = false;
  const stop = () => {
    if (closed) return;
    closed = true; clearTimeout(pollTimer);
    const player = media.querySelector('tf-video-stream');
    player?.mediaElement?.pause();
    if (player) player.remove();
    media.querySelector('img')?.removeAttribute('src');
    stream?.close(); if (localUrl) URL.revokeObjectURL(localUrl);
  };
  signal.addEventListener('abort', stop);
  const fail = (error) => { errorEl.hidden = false; errorEl.textContent = `${t('attachment_failed')}: ${attachmentError(error)}`; };
  const showSource = (url, mime) => {
    if (mime.startsWith('image/')) { media.innerHTML = `<img alt="${escapeAttr(attachment.name)}">`; media.querySelector('img').src = url; return; }
    const player = document.createElement('tf-video-stream');
    player.setAttribute('src', url); player.setAttribute('controls', ''); player.setAttribute('height-px', '420');
    player.addEventListener('media-error', () => {
      status.textContent = t('attachment_video_error'); retry.hidden = !owner.ownerId;
    });
    media.replaceChildren(player);
  };
  const convertedPreview = async (explicitRetry = false) => {
    clearTimeout(pollTimer); retry.hidden = true; errorEl.hidden = true;
    try {
      const result = await ApiBinary.one('projectStudioAttachmentPreviewRequest', { ...owner, sha256: attachment.sha256, retry: explicitRetry });
      if (closed) return;
      if (result.status === 'ready') {
        const next = await openAttachmentStream(owner, attachment, { preview: true });
        if (closed) { next.close(); return; }
        stream?.close(); stream = next; showSource(next.url, result.mime);
        status.textContent = t('attachment_preview_ready', { duration: taskDuration(Number(result.duration_ms) / 1000, t) });
      } else if (result.status === 'error') {
        status.textContent = `${t('attachment_preview_error')}: ${result.error}`; retry.hidden = false;
      } else {
        status.textContent = t(`attachment_preview_${result.status}`);
        pollTimer = setTimeout(() => convertedPreview(), 2500);
      }
    } catch (error) { if (!closed) { fail(error); retry.hidden = false; } }
  };
  retry.addEventListener('click', () => convertedPreview(true));
  const startPreview = async () => {
    if (attachment.mime.startsWith('image/') || attachment.mime.startsWith('video/')) {
    if (localFile) {
      localUrl = URL.createObjectURL(localFile); showSource(localUrl, attachment.mime);
      if (!owner.ownerId && attachment.mime.startsWith('video/')) status.textContent = t('attachment_save_first');
    } else if (attachment.mime.startsWith('video/')) convertedPreview();
    else {
      try { stream = await openAttachmentStream(owner, attachment); if (!closed) showSource(stream.url, stream.mime); else stream.close(); }
      catch (error) { if (!closed) fail(error); }
    }
    } else status.textContent = t('attachment_download_hint');
  };
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]');
    if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'close') { stop(); cleanup(); return; }
    if (localFile) {
      const url = URL.createObjectURL(localFile); downloadUrl(url, attachment.name); setTimeout(() => URL.revokeObjectURL(url), 30000); return;
    }
    button.setAttribute('disabled', '');
    try {
      const download = await downloadProjectAttachment(owner, attachment);
      const progress = body.querySelector('[data-download-progress]');
      progress.innerHTML = `<div class="ps-task-comment-tools"><span>${escapeHtml(t('attachment_download_started'))}</span><tf-button variant="ghost" data-download-cancel>${escapeHtml(t('action_cancel'))}</tf-button></div>`;
      progress.querySelector('[data-download-cancel]').addEventListener('click', () => { download.close(); progress.textContent = t('attachment_download_canceled'); });
      download.onProgress = ({ offset, total }) => {
        const caption = progress.querySelector('span');
        if (caption) caption.textContent = t('attachment_upload_bytes', { done: formatBytes(offset), total: formatBytes(total) });
      };
      download.onComplete = () => { if (progress.isConnected) progress.textContent = t('attachment_download_complete'); };
      download.onCancel = () => { if (progress.isConnected) progress.textContent = t('attachment_download_canceled'); };
      download.onError = (error) => { if (progress.isConnected) { progress.textContent = t('attachment_download_failed'); fail(error); } };
    } catch (error) { fail(error); }
    finally { button.removeAttribute('disabled'); }
  });
  startPreview();
}

function renderTaskAttachmentGallery(host, attachments, owner, { removable, onRemove }) {
  const streams = [];
  host.innerHTML = attachments.length ? attachments.map((attachment, index) => `<div class="ps-task-attachment"><div class="ps-task-attachment-image" data-gallery-image="${index}" ${attachment.mime.startsWith('image/') ? '' : 'hidden'}></div><div class="ps-task-attachment-meta"><b>${escapeHtml(attachment.name)}</b><span>${escapeHtml(attachment.mime)} · ${formatBytes(attachment.size_bytes)}</span></div><div class="ps-task-attachment-actions"><tf-button variant="ghost" size="sm" icon="${attachment.mime.startsWith('video/') ? 'play' : 'external-link'}" data-att-preview="${index}" title="${escapeAttr(t('kb_preview'))}">${escapeHtml(t('kb_preview'))}</tf-button>${removable ? `<tf-button variant="ghost" size="sm" icon="trash" data-att-remove="${index}" title="${escapeAttr(t('attachment_remove'))}"></tf-button>` : ''}</div></div>`).join('') : `<span class="ps-field-hint">${escapeHtml(t('attachments_empty'))}</span>`;
  let disposed = false;
  const showImage = async (cell) => {
    if (cell.dataset.loaded) return;
    cell.dataset.loaded = 'true';
    const attachment = attachments[Number(cell.dataset.galleryImage)];
    const localFile = owner.ownerId ? null : state.localAttachments.get(`${owner.projectId}:${attachment.sha256}`);
    let url;
    try {
      if (localFile) { url = URL.createObjectURL(localFile); streams.push({ close: () => URL.revokeObjectURL(url) }); }
      else if (owner.ownerId) { const stream = await openAttachmentStream(owner, attachment); if (disposed) { stream.close(); return; } streams.push(stream); url = stream.url; }
      else return;
      if (disposed || !cell.isConnected) return;
      const image = document.createElement('img'); image.alt = attachment.name; image.loading = 'lazy'; image.src = url; cell.replaceChildren(image);
    } catch { cell.hidden = true; }
  };
  const observer = typeof IntersectionObserver === 'function' ? new IntersectionObserver((entries) => { for (const entry of entries) if (entry.isIntersecting) { observer.unobserve(entry.target); showImage(entry.target); } }) : null;
  host.querySelectorAll('[data-gallery-image]:not([hidden])').forEach((cell) => { if (observer) observer.observe(cell); else showImage(cell); });
  host.querySelectorAll('[data-att-preview]').forEach((button) => button.addEventListener('click', () => openAttachmentPreview(attachments[Number(button.dataset.attPreview)], owner)));
  host.querySelectorAll('[data-att-remove]').forEach((button) => button.addEventListener('click', () => onRemove(Number(button.dataset.attRemove))));
  return () => { disposed = true; observer?.disconnect(); for (const stream of streams) stream.close(); };
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

const TESTS_SEGMENTS = ['cases', 'suites', 'runs', 'generations', 'environments', 'schedules', 'reports'];

const TESTS_SEG_ICON = {
  cases: 'list',
  suites: 'grid-rows',
  runs: 'play',
  generations: 'sparkle',
  environments: 'host',
  schedules: 'clock',
  reports: 'bar-chart',
};

// Drill-in views highlight their parent segment in the sub-nav.
function testsSegValue() {
  const view = f2().view;
  if (view === 'case-editor') return 'cases';
  if (view === 'suite-editor') return 'suites';
  if (view === 'run-detail' || view === 'exec' || view === 'auto-run') return 'runs';
  if (view === 'gen-detail') return 'generations';
  if (view === 'env-approvals') return 'environments';
  return TESTS_SEGMENTS.includes(view) ? view : 'cases';
}

async function renderTests() {
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  panel.innerHTML = `
    <div class="ps-subnav">
      <tf-segmented id="ps-tests-seg" value="${escapeAttr(testsSegValue())}">
        ${TESTS_SEGMENTS.filter((seg) => seg !== 'environments' || canArea('environments')).map((seg) => `<option value="${seg}" icon="${TESTS_SEG_ICON[seg]}">${escapeHtml(t(`tests_seg_${seg}`))}</option>`).join('')}
      </tf-segmented>
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
  if (['environments', 'env-approvals'].includes(f2().view) && !canArea('environments')) f2().view = 'cases';
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
    case 'auto-run': await renderAutoRunView(); break;
    case 'exec': await renderExecView(); break;
    case 'generations': await renderGenerationsView(); break;
    case 'gen-detail': await renderGenDetailView(); break;
    case 'environments': await renderEnvironmentsView(); break;
    case 'env-approvals': await renderEnvApprovalsView(); break;
    case 'schedules': await renderSchedulesView(); break;
    case 'reports': await renderReportsView(); break;
    default: f2().view = 'cases'; await renderCasesView(); break;
  }
}

// Tears down every live artifact of the tests tab: the generation event
// stream, the generation poll, the execution duration ticker and the F3 live
// streams (automated run, try run, code assist).
function stopTestsLive() {
  const s = state.f2;
  if (!s) return;
  if (s.genUnsub) { try { s.genUnsub(); } catch { /* stream already gone */ } s.genUnsub = null; }
  if (s.genPollTimer) { clearInterval(s.genPollTimer); s.genPollTimer = null; }
  s.genWidget = null;
  s.genSteps = [];
  if (s.exec?.timerId) { clearInterval(s.exec.timerId); s.exec.timerId = null; }
  stopAutoRunLive();
  stopCodeEditorLive();
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
    </div>
    ${canArea('tests', 'write') ? `
      <div class="ps-tests-toolbar ps-cases-actions">
        <tf-button variant="ghost" icon="download" id="ps-cases-import">${escapeHtml(t('cases_import_csv'))}</tf-button>
        <tf-button variant="ghost" icon="sparkle" id="ps-cases-generate" ${canGenerateTests() ? '' : 'disabled'}>${escapeHtml(t('cases_generate'))}</tf-button>
        <span class="ps-new-case-wrap">
          <tf-button variant="primary" icon="plus" id="ps-cases-new">${escapeHtml(t('cases_new'))}</tf-button>
          <tf-menu placement="bottom-end" id="ps-cases-new-menu">
            ${CASE_KINDS.map((k) => `<tf-menu-item action="${k}" icon="${k === 'manual' ? 'list' : 'code'}">${escapeHtml(t(`case_kind_${k}`))}</tf-menu-item>`).join('')}
          </tf-menu>
        </span>
      </div>
    ` : ''}
    <div class="ps-bulk-bar" id="ps-cases-bulk" hidden>
      <span class="ps-bulk-count" id="ps-cases-bulk-count"></span>
      ${canArea('tests', 'write') ? `<tf-button variant="ghost" size="sm" icon="send" data-bulk="review">${escapeHtml(t('bulk_to_review'))}</tf-button>` : ''}
      ${canArea('tests', 'admin') ? `<tf-button variant="ghost" size="sm" icon="check" data-bulk="approved">${escapeHtml(t('bulk_approve'))}</tf-button>` : ''}
      ${canArea('tests', 'admin') ? `<tf-button variant="ghost" size="sm" icon="ban" data-bulk="deprecated">${escapeHtml(t('bulk_deprecate'))}</tf-button>` : ''}
      ${canArea('tests', 'write') ? `<tf-button variant="ghost" size="sm" icon="trash" data-bulk="delete">${escapeHtml(t('bulk_delete'))}</tf-button>` : ''}
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
  // The kind is fixed at creation (the server refuses to change it later), so
  // "new case" opens the kind menu instead of assuming 'manual'.
  byId('ps-cases-new')?.addEventListener('click', (e) => {
    e.stopPropagation();
    byId('ps-cases-new-menu')?.toggle();
  });
  byId('ps-cases-new-menu')?.addEventListener('action', (e) => {
    const kind = e.detail?.action;
    if (kind && CASE_KINDS.includes(kind)) openCaseEditor(null, kind);
  });
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
      <tf-column key="title" label="${escapeAttr(t('cases_col_title'))}" renderer="html" sortable></tf-column>
      <tf-column key="kind" label="${escapeAttr(t('cases_col_kind'))}" renderer="chip"></tf-column>
      <tf-column key="priority" label="${escapeAttr(t('cases_col_priority'))}" renderer="chip"></tf-column>
      <tf-column key="tags" label="${escapeAttr(t('cases_col_tags'))}"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('cases_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="author" label="${escapeAttr(t('cases_col_author'))}"></tf-column>
      <tf-column key="origin" label="${escapeAttr(t('cases_col_origin'))}" renderer="chip"></tf-column>
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
        _selected: s.cases.selected.has(caseId),
        title: `<div class="tf-table__cell-title">${escapeHtml(c.title)}</div>`
          + `<div class="tf-table__cell-sub">${escapeHtml(shortId(caseId))}</div>`,
        kind: chipCell('info', t(`case_kind_${c.kind}`)),
        priority: chipCell(PRIORITY_CHIP[c.priority], t(`prio_${c.priority}`)),
        tags: (fv(c, 'tag_ids') || []).map(tagNameById).filter(Boolean).join(', ') || '—',
        status: chipCell(CASE_STATUS_CHIP[c.status], t(`case_status_${c.status}`)),
        // A <use> reference cannot resolve document symbols from inside the
        // table's shadow root, so this column carries a label, not an icon.
        author: fv(c, 'created_by_name') || '—',
        origin: c.origin === 'agent'
          ? chipCell('accent', t('origin_agent'))
          : chipCell('info', t('origin_user')),
        lastResult: lastResult
          ? chipCell(ITEM_STATUS_CHIP[lastResult], t(`item_status_${lastResult}`))
          : chipCell('info', '—'),
        updated: formatTimestamp(fv(c, 'updated_at')),
      };
    });
  };
  assignRows();
  table.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
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
    mk('edit', t(canArea('tests', 'write') ? 'action_edit' : 'cases_open'), () => openCaseEditor(live()._id));
    if (canArea('tests', 'write')) {
      mk('copy', t('cases_duplicate'), () => duplicateCase(live()._id));
      mk('trash', t('action_delete'), () => deleteCaseFromList(live()._row));
    }
    return wrap;
  };
  table.addEventListener('row-click', (e) => {
    const caseId = e.detail?.row?._id;
    if (caseId) openCaseEditor(caseId);
  });
  table.addEventListener('row-select', (e) => {
    const caseId = e.detail?.row?._id;
    if (!caseId) return;
    if (e.detail.selected) s.cases.selected.add(caseId);
    else s.cases.selected.delete(caseId);
    updateCasesBulkBar();
    syncCasesFooter();
  });
  table.addEventListener('select-all', (e) => {
    if (e.detail?.selected) s.cases.rows.forEach((c) => s.cases.selected.add(fv(c, 'case_id')));
    else s.cases.selected = new Set();
    assignRows();
    updateCasesBulkBar();
    syncCasesFooter();
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
    syncCasesFooter();
  });
  syncCasesFooter();
  updateCasesBulkBar();
}

// Task links are stored as a JSON array of {kind, id, label}; the list view
// shows their labels so a defect's origin is visible without opening it.
function taskLinkLabels(task) {
  try {
    const parsed = JSON.parse(fv(task, 'links_json') || '[]');
    if (!Array.isArray(parsed) || !parsed.length) return '—';
    return parsed.map((l) => l.label || l.id || '').filter(Boolean).join(', ') || '—';
  } catch {
    return '—';
  }
}

function syncCasesFooter() {
  const s = f2();
  const host = byId('ps-cases-table-host');
  if (!host) return;
  let footer = host.querySelector('.ps-table-footer');
  if (!footer) {
    footer = document.createElement('div');
    footer.className = 'ps-table-footer';
    host.appendChild(footer);
  }
  footer.textContent = t('cases_footer', {
    shown: s.cases.rows.length,
    total: s.cases.total,
    selected: s.cases.selected.size,
  });
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

// `kind` only matters for a NEW case: an existing one carries its own kind,
// and the server refuses to change it on save.
function openCaseEditor(caseId, kind = 'manual') {
  stopCodeEditorLive();
  f2().editor = {
    caseId: caseId || null,
    kind,
    loaded: !caseId,
    info: null,
    title: '',
    priority: 'medium',
    preconditions: '',
    testData: '',
    steps: caseId ? [] : [{ action: '', expected: '' }],
    // Code kinds (T03): script + per-kind configuration.
    script: '',
    language: 'python',
    config: {},
    checklist: [],
    buildProfileRef: '',
    envId: '',
    tryRun: null,
    assist: null,
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
    script: String(content.script ?? ''),
    language: String(content.language ?? '') || 'python',
    config: content.config && typeof content.config === 'object' ? content.config : {},
    profile: content.profile && typeof content.profile === 'object' ? content.profile : {},
    checklist: Array.isArray(content.checklist) ? content.checklist.map(String) : [],
    buildProfileRef: String(content.build_profile_ref ?? content.buildProfileRef ?? ''),
  };
}

function isCodeKind(kind) {
  return CODE_KINDS.includes(kind);
}

// tf-code-editor highlights by language id; the runner language maps 1:1 for
// the executable set and degrades to plain text for anything else.
function editorLanguageOf(language) {
  const lang = String(language || '').toLowerCase();
  return ['python', 'javascript', 'typescript', 'json', 'yaml', 'markdown', 'gherkin'].includes(lang) ? lang : 'plain';
}

function codeEditorLabels() {
  return {
    editor: t('ce_editor'),
    find: t('ce_find'),
    replace: t('ce_replace'),
    match_case: t('ce_match_case'),
    regex: t('ce_regex'),
    prev: t('ce_prev'),
    next: t('ce_next'),
    replace_one: t('ce_replace_one'),
    replace_all: t('ce_replace_all'),
    close: t('action_close'),
    matches: t('ce_matches'),
    no_matches: t('ce_no_matches'),
    bad_regex: t('ce_bad_regex'),
    folded_lines: t('ce_folded_lines'),
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
  ed.kind = info.kind || ed.kind;
  ed.title = info.title || '';
  ed.priority = info.priority || 'medium';
  ed.preconditions = content.preconditions;
  ed.testData = content.testData;
  ed.steps = content.steps;
  ed.script = content.script;
  ed.language = (info.language && isCodeKind(ed.kind)) ? info.language : content.language;
  ed.config = ed.kind === 'perf' ? content.profile : content.config;
  ed.checklist = content.checklist;
  ed.buildProfileRef = content.buildProfileRef;
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
  if (canArea('knowledge') && !state.sources.length) {
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

  const codeCase = isCodeKind(ed.kind);
  if (codeCase) {
    await loadRunners();
    if (canArea('environments')) {
      try { await loadEnvironments(); } catch { /* the try-run guard reports it */ }
    }
    if (state.tab !== 'tests' || f2().view !== 'case-editor') return;
    const approved = approvedEnvironments();
    if (!ed.envId && approved.length === 1) ed.envId = fv(approved[0], 'environment_id');
  }
  const info = ed.info;
  const status = info?.status || 'draft';
  const editable = canArea('tests', 'write') && (!info || status === 'draft' || status === 'review');
  const statusActions = [];
  if (info) {
    if (status === 'draft' && canArea('tests', 'write')) {
      statusActions.push(`<tf-button variant="ghost" icon="send" data-status="review">${escapeHtml(t('case_send_review'))}</tf-button>`);
    }
    if (status === 'review' && canArea('tests', 'admin')) {
      statusActions.push(`<tf-button variant="ghost" icon="check" data-status="approved">${escapeHtml(t('case_approve'))}</tf-button>`);
    }
    if (status === 'review' && canArea('tests', 'admin')) {
      statusActions.push(`<tf-button variant="ghost" icon="chevron-left" data-status="draft" data-needs-reason>${escapeHtml(t('case_back_to_draft'))}</tf-button>`);
    }
    if (status === 'approved' && canArea('tests', 'admin')) {
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
        <tf-chip status="${codeCase ? 'accent' : 'info'}">${escapeHtml(t(`case_kind_${ed.kind}`))}</tf-chip>
        ${info?.origin === 'agent' ? `<tf-chip status="accent">${sprite('sparkle')} ${escapeHtml(t('origin_agent'))}</tf-chip>` : ''}
      </div>
      <div class="ps-editor-actions">
        ${statusActions.join('')}
        ${editable ? `<tf-button variant="primary" icon="check" id="ps-case-save">${escapeHtml(t('action_save'))}</tf-button>` : ''}
        ${info && canArea('tests', 'write') ? `<tf-button variant="ghost" icon="trash" id="ps-case-delete" title="${escapeAttr(t('action_delete'))}"></tf-button>` : ''}
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

        ${codeCase ? codeCaseSectionsHtml(ed, editable) : `
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
        `}

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
  byId('ps-case-back')?.addEventListener('click', () => {
    stopCodeEditorLive();
    f2().editor = null;
    f2().view = 'cases';
    renderTestsView();
  });
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

  if (codeCase) wireCodeCaseEditor(ed, editable);
  else renderCaseSteps(editable);
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
    btn.addEventListener('click', () => openAttachmentPreview(ed.attachments[Number(btn.dataset.attPreview)], attachmentOwner('case', ed.caseId)));
  });
  host.querySelectorAll('[data-att-remove]').forEach((btn) => {
    btn.addEventListener('click', () => {
      ed.attachments.splice(Number(btn.dataset.attRemove), 1);
      renderCaseAttachments(editable);
    });
  });
}

// =============================================================================
// T03 — code case editor (tf-code-editor + AI assist + try run)
// =============================================================================

// Per-kind configuration block of a code case; the field set mirrors the
// content_json contract validated by generation::validate_case_content.
function codeConfigHtml(ed, editable) {
  const cfg = ed.config || {};
  const ro = editable ? '' : 'readonly';
  const viewport = cfg.viewport && typeof cfg.viewport === 'object' ? cfg.viewport : {};
  if (ed.kind === 'ui') {
    return `
      <div class="ps-code-config">
        <tf-input id="ps-code-vw" type="number" min="120" max="8000" label="${escapeAttr(t('code_cfg_viewport_w'))}" value="${escapeAttr(String(viewport.width ?? 1280))}" ${ro}></tf-input>
        <tf-input id="ps-code-vh" type="number" min="120" max="8000" label="${escapeAttr(t('code_cfg_viewport_h'))}" value="${escapeAttr(String(viewport.height ?? 720))}" ${ro}></tf-input>
        <tf-input id="ps-code-timeout" type="number" min="100" max="600000" label="${escapeAttr(t('code_cfg_timeout'))}" value="${escapeAttr(String(cfg.timeout_ms ?? 30000))}" ${ro}></tf-input>
        <label class="ps-toggle-field">
          <span class="ps-field-label">${escapeHtml(t('code_cfg_headed'))}</span>
          <tf-toggle id="ps-code-headed" ${cfg.headed ? 'checked' : ''} ${editable ? '' : 'disabled'}></tf-toggle>
        </label>
      </div>
    `;
  }
  if (ed.kind === 'api') {
    return `
      <div class="ps-code-config">
        <tf-input id="ps-code-timeout" type="number" min="100" max="600000" label="${escapeAttr(t('code_cfg_timeout'))}" value="${escapeAttr(String(cfg.timeout_ms ?? 30000))}" ${ro}></tf-input>
      </div>
    `;
  }
  if (ed.kind === 'perf') {
    return `
      <div class="ps-code-config">
        <tf-input id="ps-code-users" type="number" min="${PERF_LIMITS.users[0]}" max="${PERF_LIMITS.users[1]}" label="${escapeAttr(t('code_cfg_users'))}" value="${escapeAttr(String(cfg.users ?? PERF_DEFAULT_PROFILE.users))}" ${ro}></tf-input>
        <tf-input id="ps-code-spawn" type="number" min="${PERF_LIMITS.spawnRate[0]}" max="${PERF_LIMITS.spawnRate[1]}" label="${escapeAttr(t('code_cfg_spawn'))}" value="${escapeAttr(String(cfg.spawn_rate ?? PERF_DEFAULT_PROFILE.spawn_rate))}" ${ro}></tf-input>
        <tf-input id="ps-code-duration" type="number" min="${PERF_LIMITS.duration[0]}" max="${PERF_LIMITS.duration[1]}" label="${escapeAttr(t('code_cfg_duration'))}" value="${escapeAttr(String(cfg.duration_secs ?? PERF_DEFAULT_PROFILE.duration_secs))}" ${ro}></tf-input>
      </div>
    `;
  }
  if (ed.kind === 'unit') {
    return `
      <div class="ps-code-config">
        <tf-input id="ps-code-build-ref" label="${escapeAttr(t('code_cfg_build_profile'))}" value="${escapeAttr(ed.buildProfileRef)}"
          hint="${escapeAttr(t('code_cfg_build_profile_hint'))}" ${ro}></tf-input>
      </div>
    `;
  }
  return `
    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('code_cfg_checklist'))}</span>
      <tf-tag-input id="ps-code-checklist" dedupe placeholder="${escapeAttr(t('code_cfg_checklist_placeholder'))}" ${editable ? '' : 'disabled'}></tf-tag-input>
      <div class="ps-field-hint">${escapeHtml(t('code_cfg_checklist_hint'))}</div>
    </div>
  `;
}

function codeCaseSectionsHtml(ed, editable) {
  const advertised = runnerLanguages();
  const approved = approvedEnvironments();
  const quickActions = ['generate', 'fix', 'explain', 'assertions'];
  return `
    <tf-section-card title="${escapeAttr(t('code_script_title'))}" icon="code">
      <span slot="subtitle">${escapeHtml(t('code_script_sub'))}</span>
      <div class="ps-code-toolbar">
        <tf-select id="ps-code-language" label="${escapeAttr(t('code_language_label'))}" value="${escapeAttr(ed.language)}" ${editable ? '' : 'disabled'}>
          ${CODE_LANGUAGES.map((lang) => {
            const ready = lang.executable && advertised.has(lang.id);
            const suffix = lang.executable
              ? (ready ? t('code_language_ready') : t('code_language_no_runner'))
              : t('code_language_unavailable');
            return `<option value="${lang.id}" ${lang.id === ed.language ? 'selected' : ''} ${lang.executable ? '' : 'disabled'}>${escapeHtml(`${lang.id} — ${suffix}`)}</option>`;
          }).join('')}
        </tf-select>
        ${canArea('environments') ? `<tf-select id="ps-code-env" label="${escapeAttr(t('code_env_label'))}" value="${escapeAttr(ed.envId)}">
          <option value="">${escapeHtml(approved.length ? t('assign_choose') : t('code_env_missing'))}</option>
          ${approved.map((env) => `<option value="${escapeAttr(fv(env, 'environment_id'))}" ${fv(env, 'environment_id') === ed.envId ? 'selected' : ''}>${escapeHtml(`${env.name} — ${fv(env, 'base_url')}`)}</option>`).join('')}
        </tf-select>` : ''}
        <span class="ps-toolbar-spacer"></span>
        ${canArea('tests', 'write') ? `
          <tf-button variant="ghost" icon="play" id="ps-code-try" ${canTryRun() ? '' : `disabled title="${escapeAttr(`${areaLabel('environments')} · ${t('access_level_none')}`)}"`}>${escapeHtml(t('code_try_run'))}</tf-button>
          <tf-button variant="ghost" icon="stop" id="ps-code-try-stop" hidden>${escapeHtml(t('code_try_stop'))}</tf-button>
        ` : ''}
      </div>
      <div class="ps-code-editor-host" id="ps-code-editor-host"></div>
      <div class="ps-field-hint">${escapeHtml(t('code_editor_hint'))}</div>
    </tf-section-card>

    <tf-section-card title="${escapeAttr(t('code_config_title'))}" icon="settings">
      ${codeConfigHtml(ed, editable)}
    </tf-section-card>

    ${canArea('tests', 'write') ? `
      <tf-section-card class="ps-ai-panel" title="${escapeAttr(t('ai_panel_title'))}" icon="sparkle">
        <span slot="subtitle">${escapeHtml(t('ai_panel_sub'))}</span>
        <div class="ps-ai-chips" id="ps-ai-chips">
          ${quickActions.map((id) => `<tf-chip status="accent" data-ai-quick="${id}" role="button" tabindex="0">${escapeHtml(t(`ai_quick_${id}`))}</tf-chip>`).join('')}
        </div>
        <div class="ps-ai-ask">
          <tf-input id="ps-ai-instruction" label="${escapeAttr(t('ai_instruction_label'))}" placeholder="${escapeAttr(t('ai_instruction_placeholder'))}"></tf-input>
          <tf-button variant="primary" icon="sparkle" id="ps-ai-ask">${escapeHtml(t('ai_ask'))}</tf-button>
          <tf-button variant="ghost" icon="stop" id="ps-ai-stop" hidden>${escapeHtml(t('ai_stop'))}</tf-button>
        </div>
        <div class="ps-ai-scope" id="ps-ai-scope">${escapeHtml(t('ai_scope_whole'))}</div>
        <div class="ps-ai-stream" id="ps-ai-stream" hidden></div>
        <div class="ps-ai-proposal" id="ps-ai-proposal" hidden></div>
      </tf-section-card>
    ` : ''}

    <tf-section-card title="${escapeAttr(t('code_try_title'))}" icon="prompt">
      <span slot="subtitle">${escapeHtml(t('code_try_hint'))}</span>
      <div class="ps-live-log" id="ps-code-console">${escapeHtml(t('code_console_empty'))}</div>
    </tf-section-card>
  `;
}

function wireCodeCaseEditor(ed, editable) {
  const hostEl = byId('ps-code-editor-host');
  if (!hostEl) return;
  const editor = document.createElement('tf-code-editor');
  editor.setAttribute('language', editorLanguageOf(ed.language));
  editor.setAttribute('aria-label', t('ce_editor'));
  if (!editable) editor.setAttribute('readonly', '');
  editor.labels = codeEditorLabels();
  editor.value = ed.script;
  hostEl.replaceChildren(editor);
  ed.editorEl = editor;

  editor.addEventListener('change', (e) => { ed.script = String(e.detail?.value ?? editor.value ?? ''); });
  editor.addEventListener('save', () => { if (editable) saveCaseFromEditor(); });
  editor.addEventListener('selection-change', (e) => {
    const scope = byId('ps-ai-scope');
    if (!scope) return;
    const text = String(e.detail?.text ?? '');
    scope.textContent = text.trim()
      ? t('ai_scope_selection', { chars: text.length })
      : t('ai_scope_whole');
  });

  byId('ps-code-language')?.addEventListener('change', (e) => {
    ed.language = e.detail?.value ?? e.target.value ?? ed.language;
    editor.setAttribute('language', editorLanguageOf(ed.language));
  });
  byId('ps-code-env')?.addEventListener('change', (e) => {
    ed.envId = e.detail?.value ?? e.target.value ?? '';
  });

  const checklistInput = byId('ps-code-checklist');
  if (checklistInput) {
    checklistInput.tags = (ed.checklist || []).slice();
    checklistInput.addEventListener('change', (e) => {
      ed.checklist = Array.isArray(e.detail?.tags) ? e.detail.tags : checklistInput.tags;
    });
  }

  byId('ps-code-try')?.addEventListener('click', () => startTryRun(ed));
  byId('ps-code-try-stop')?.addEventListener('click', () => cancelTryRun(ed));
  byId('ps-ai-ask')?.addEventListener('click', () => startCodeAssist(ed));
  byId('ps-ai-stop')?.addEventListener('click', () => stopCodeAssist());
  byId('ps-ai-chips')?.addEventListener('click', (e) => {
    const chip = e.target.closest('[data-ai-quick]');
    if (!chip) return;
    const input = byId('ps-ai-instruction');
    if (input) input.value = t(`ai_quick_${chip.dataset.aiQuick}_prompt`);
    startCodeAssist(ed);
  });

  // A restored console keeps the last try-run output visible after a re-render.
  const consoleEl = byId('ps-code-console');
  if (consoleEl && ed.tryRun?.log?.length) consoleEl.textContent = ed.tryRun.log.join('\n');
}

// Reads the per-kind config back out of the DOM into the editor state so both
// save and try-run see the same content.
function collectCodeConfig(ed) {
  const num = (id, dflt) => {
    const raw = byId(id)?.value;
    const value = Number(raw);
    return Number.isFinite(value) && String(raw ?? '').trim() !== '' ? value : dflt;
  };
  if (ed.kind === 'ui') {
    ed.config = {
      viewport: { width: num('ps-code-vw', 1280), height: num('ps-code-vh', 720) },
      timeout_ms: num('ps-code-timeout', 30000),
      headed: !!byId('ps-code-headed')?.checked,
    };
  } else if (ed.kind === 'api') {
    ed.config = { timeout_ms: num('ps-code-timeout', 30000) };
  } else if (ed.kind === 'perf') {
    ed.config = {
      users: num('ps-code-users', PERF_DEFAULT_PROFILE.users),
      spawn_rate: num('ps-code-spawn', PERF_DEFAULT_PROFILE.spawn_rate),
      duration_secs: num('ps-code-duration', PERF_DEFAULT_PROFILE.duration_secs),
    };
  } else if (ed.kind === 'unit') {
    ed.buildProfileRef = String(byId('ps-code-build-ref')?.value ?? '').trim();
  } else if (ed.kind === 'security') {
    const checklist = byId('ps-code-checklist');
    if (Array.isArray(checklist?.tags)) ed.checklist = checklist.tags;
  }
  if (ed.editorEl) ed.script = String(ed.editorEl.value ?? ed.script);
}

// content_json of a code case, per the kind contract.
function codeContentJson(ed) {
  const content = { script: ed.script, language: ed.language };
  if (ed.kind === 'ui' || ed.kind === 'api') content.config = ed.config;
  else if (ed.kind === 'perf') content.profile = ed.config;
  else if (ed.kind === 'unit') { if (ed.buildProfileRef) content.build_profile_ref = ed.buildProfileRef; }
  else if (ed.kind === 'security') content.checklist = ed.checklist || [];
  return JSON.stringify(content);
}

function appendTryLog(ed, line) {
  if (!ed.tryRun) return;
  ed.tryRun.log.push(line);
  if (ed.tryRun.log.length > TRY_LOG_CAP) ed.tryRun.log.splice(0, ed.tryRun.log.length - TRY_LOG_CAP);
  const el = byId('ps-code-console');
  if (el) {
    el.textContent = ed.tryRun.log.join('\n');
    el.scrollTop = el.scrollHeight;
  }
}

function setTryRunButtons(running) {
  const start = byId('ps-code-try');
  const stop = byId('ps-code-try-stop');
  if (start) start.hidden = running;
  if (stop) stop.hidden = !running;
}

// T03 "Uruchom próbnie": ephemeral execution of the UNSAVED editor content
// against an approved environment. Nothing is persisted as a run.
async function startTryRun(ed) {
  if (ed.tryRun?.running || !canTryRun()) return;
  collectCodeConfig(ed);
  if (!ed.caseId) { toast(t('code_try_needs_save'), 'error'); return; }
  if (!ed.script.trim()) { toast(t('err_case_script'), 'error'); return; }
  if (!ed.envId) { toast(t('code_try_needs_env'), 'error'); return; }

  const tryId = crypto.randomUUID();
  ed.tryRun = { id: tryId, running: true, log: [], unsub: null };
  setTryRunButtons(true);
  appendTryLog(ed, t('code_try_started'));

  const perfProfileJson = ed.kind === 'perf' ? JSON.stringify(ed.config) : '';
  try {
    const unsub = await ApiBinary.subscribe(
      'projectStudioTryRunStartRequest',
      {
        projectId: projectId(),
        tryId,
        caseId: ed.caseId,
        environmentId: ed.envId,
        contentJsonOverride: codeContentJson(ed),
        language: ed.language,
        perfProfileJson,
      },
      {
        onChunk: (body) => {
          if (body?.variant !== 'ProjectStudioTryRunStreamChunk') return;
          if (f2().editor !== ed) return;
          appendTryLog(ed, body.phase ? `[${body.phase}] ${body.line}` : String(body.line || ''));
        },
        onEnd: (body) => {
          if (f2().editor !== ed) return;
          const status = body?.status || 'completed';
          const error = body?.error;
          appendTryLog(ed, error ? `${t('code_try_failed')}: ${error}` : t('code_try_done', { status }));
          const summary = fv(body ?? {}, 'junit_summary_json');
          if (summary && summary !== '{}') appendTryLog(ed, summary);
          ed.tryRun.running = false;
          setTryRunButtons(false);
        },
        onError: (body) => {
          if (f2().editor !== ed) return;
          appendTryLog(ed, `${t('code_try_failed')}: ${body?.message ?? ''}`);
          ed.tryRun.running = false;
          setTryRunButtons(false);
        },
      },
    );
    if (f2().editor !== ed || !ed.tryRun.running) { unsub(); return; }
    ed.tryRun.unsub = unsub;
  } catch (err) {
    ed.tryRun.running = false;
    setTryRunButtons(false);
    appendTryLog(ed, `${t('code_try_failed')}: ${err.message}`);
  }
}

async function cancelTryRun(ed) {
  const run = ed.tryRun;
  if (!run?.running) return;
  try {
    await ApiBinary.one('projectStudioTryRunCancelRequest', { projectId: projectId(), tryId: run.id });
    appendTryLog(ed, t('code_try_cancelled'));
  } catch (err) {
    appendTryLog(ed, `${t('code_try_failed')}: ${err.message}`);
  }
  run.running = false;
  if (run.unsub) { try { run.unsub(); } catch { /* stream already gone */ } run.unsub = null; }
  setTryRunButtons(false);
}

// T03 AI assist: streams a proposal through the project's generator agent and
// shows it as a two-column diff before anything touches the editor buffer.
async function startCodeAssist(ed) {
  const assist = ed.assist;
  if (assist?.busy) return;
  collectCodeConfig(ed);
  const instruction = String(byId('ps-ai-instruction')?.value ?? '').trim();
  if (!instruction) { toast(t('ai_empty_instruction'), 'error'); return; }
  const selection = ed.editorEl?.getSelection?.()?.text ?? '';

  ed.assist = { busy: true, unsub: null, stream: '', selection, instruction };
  const streamEl = byId('ps-ai-stream');
  const proposalEl = byId('ps-ai-proposal');
  if (proposalEl) { proposalEl.hidden = true; proposalEl.innerHTML = ''; }
  if (streamEl) {
    streamEl.hidden = false;
    streamEl.textContent = t('ai_streaming');
  }
  byId('ps-ai-ask')?.setAttribute('disabled', '');
  const stopBtn = byId('ps-ai-stop');
  if (stopBtn) stopBtn.hidden = false;

  const finish = () => {
    ed.assist.busy = false;
    byId('ps-ai-ask')?.removeAttribute('disabled');
    const stop = byId('ps-ai-stop');
    if (stop) stop.hidden = true;
  };

  try {
    const unsub = await ApiBinary.subscribe(
      'projectStudioCodeAssistRequest',
      {
        projectId: projectId(),
        caseId: ed.caseId || '',
        kind: ed.kind,
        selection,
        instruction,
        fullContent: ed.script,
      },
      {
        onChunk: (body) => {
          if (body?.variant !== 'ProjectStudioCodeAssistStreamChunk') return;
          if (f2().editor !== ed) return;
          ed.assist.stream += String(body.token || '');
          const el = byId('ps-ai-stream');
          if (el) {
            el.textContent = ed.assist.stream;
            el.scrollTop = el.scrollHeight;
          }
        },
        onEnd: (body) => {
          if (f2().editor !== ed) return;
          finish();
          const el = byId('ps-ai-stream');
          if (el) el.hidden = true;
          if (body?.error) {
            toast(`${t('ai_failed')}: ${body.error}`, 'error');
            return;
          }
          renderAssistProposal(ed, String(body?.proposal ?? ed.assist.stream));
        },
        onError: (body) => {
          if (f2().editor !== ed) return;
          finish();
          toast(`${t('ai_failed')}: ${body?.message ?? ''}`, 'error');
        },
      },
    );
    if (f2().editor !== ed || !ed.assist.busy) { unsub(); return; }
    ed.assist.unsub = unsub;
  } catch (err) {
    finish();
    toast(`${t('ai_failed')}: ${err.message}`, 'error');
  }
}

function stopCodeAssist() {
  const ed = state.f2?.editor;
  if (!ed?.assist) return;
  if (ed.assist.unsub) { try { ed.assist.unsub(); } catch { /* stream already gone */ } ed.assist.unsub = null; }
  ed.assist.busy = false;
  byId('ps-ai-ask')?.removeAttribute('disabled');
  const stop = byId('ps-ai-stop');
  if (stop) stop.hidden = true;
  const el = byId('ps-ai-stream');
  if (el) el.hidden = true;
}

// Two-column "current / proposed" view. The comparison base is the selection
// when there was one (the agent then returns only that fragment), otherwise the
// whole script — the same contract the server prompt states.
function renderAssistProposal(ed, proposal) {
  const hostEl = byId('ps-ai-proposal');
  if (!hostEl) return;
  const clean = String(proposal || '').trim();
  if (!clean) {
    toast(t('ai_empty_proposal'), 'error');
    return;
  }
  ed.assist.proposal = clean;
  const scoped = !!ed.assist.selection.trim();
  const current = scoped ? ed.assist.selection : ed.script;
  const rows = diffLines(current, clean);

  hostEl.hidden = false;
  hostEl.innerHTML = `
    <div class="ps-diff-head">
      <span class="ps-diff-title">${escapeHtml(t('ai_proposal_title'))}</span>
      <tf-chip status="info">${escapeHtml(scoped ? t('ai_scope_selection', { chars: current.length }) : t('ai_scope_whole'))}</tf-chip>
    </div>
    <div class="ps-diff-cols">
      <div class="ps-diff-col">
        <div class="ps-diff-col-head">${escapeHtml(t('ai_diff_current'))}</div>
        ${rows.map((row) => `<div class="ps-diff-line ${row.left == null ? 'is-empty' : ''} ${row.type === 'del' || row.type === 'mod' ? 'is-del' : ''}">${escapeHtml(row.left ?? '')}</div>`).join('')}
      </div>
      <div class="ps-diff-col">
        <div class="ps-diff-col-head">${escapeHtml(t('ai_diff_proposed'))}</div>
        ${rows.map((row) => `<div class="ps-diff-line ${row.right == null ? 'is-empty' : ''} ${row.type === 'add' || row.type === 'mod' ? 'is-add' : ''}">${escapeHtml(row.right ?? '')}</div>`).join('')}
      </div>
    </div>
    <div class="ps-diff-actions">
      <tf-button variant="ghost" icon="copy" data-diff-action="copy">${escapeHtml(t('ai_copy'))}</tf-button>
      <tf-button variant="ghost" icon="x" data-diff-action="reject">${escapeHtml(t('ai_reject'))}</tf-button>
      <tf-button variant="primary" icon="check" data-diff-action="apply">${escapeHtml(t('ai_apply'))}</tf-button>
    </div>
  `;
  hostEl.querySelectorAll('[data-diff-action]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const action = btn.dataset.diffAction;
      if (action === 'reject') {
        hostEl.hidden = true;
        hostEl.innerHTML = '';
        return;
      }
      if (action === 'copy') {
        navigator.clipboard?.writeText(clean).then(
          () => toast(t('ai_copied'), 'success'),
          () => toast(t('ai_copy_failed'), 'error'),
        );
        return;
      }
      applyAssistProposal(ed, clean, scoped);
      hostEl.hidden = true;
      hostEl.innerHTML = '';
    });
  });
}

function applyAssistProposal(ed, proposal, scoped) {
  if (!ed.editorEl) return;
  if (scoped) ed.editorEl.replaceSelection(proposal);
  else ed.editorEl.value = proposal;
  ed.script = String(ed.editorEl.value ?? proposal);
  toast(t('ai_applied'), 'success');
}

// Minimal LCS line diff for the proposal view. Both sides are bounded by the
// assist limits, and the quadratic table is skipped for large inputs (the view
// then degrades to a plain side-by-side listing, still readable).
function diffLines(currentText, proposedText) {
  const left = String(currentText).split('\n');
  const right = String(proposedText).split('\n');
  const MAX_LCS_LINES = 600;
  if (left.length > MAX_LCS_LINES || right.length > MAX_LCS_LINES) {
    const rows = [];
    for (let i = 0; i < Math.max(left.length, right.length); i += 1) {
      const l = i < left.length ? left[i] : null;
      const r = i < right.length ? right[i] : null;
      rows.push({ left: l, right: r, type: l === r ? 'same' : 'mod' });
    }
    return rows;
  }
  const table = Array.from({ length: left.length + 1 }, () => new Uint32Array(right.length + 1));
  for (let i = left.length - 1; i >= 0; i -= 1) {
    for (let j = right.length - 1; j >= 0; j -= 1) {
      table[i][j] = left[i] === right[j]
        ? table[i + 1][j + 1] + 1
        : Math.max(table[i + 1][j], table[i][j + 1]);
    }
  }
  const rows = [];
  let i = 0;
  let j = 0;
  while (i < left.length && j < right.length) {
    if (left[i] === right[j]) {
      rows.push({ left: left[i], right: right[j], type: 'same' });
      i += 1;
      j += 1;
    } else if (table[i + 1][j] >= table[i][j + 1]) {
      rows.push({ left: left[i], right: null, type: 'del' });
      i += 1;
    } else {
      rows.push({ left: null, right: right[j], type: 'add' });
      j += 1;
    }
  }
  while (i < left.length) { rows.push({ left: left[i], right: null, type: 'del' }); i += 1; }
  while (j < right.length) { rows.push({ left: null, right: right[j], type: 'add' }); j += 1; }
  return rows;
}

function stopCodeEditorLive() {
  const ed = state.f2?.editor;
  if (!ed) return;
  if (ed.tryRun?.unsub) { try { ed.tryRun.unsub(); } catch { /* stream already gone */ } ed.tryRun.unsub = null; }
  if (ed.tryRun) ed.tryRun.running = false;
  if (ed.assist?.unsub) { try { ed.assist.unsub(); } catch { /* stream already gone */ } ed.assist.unsub = null; }
  if (ed.assist) ed.assist.busy = false;
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
  const codeCase = isCodeKind(ed.kind);
  let contentJson;
  if (codeCase) {
    collectCodeConfig(ed);
    if (!ed.script.trim()) {
      toast(t('err_case_script'), 'error');
      return;
    }
    contentJson = codeContentJson(ed);
  } else {
    const steps = ed.steps
      .map((s) => ({ action: s.action.trim(), expected: s.expected.trim() }))
      .filter((s) => s.action || s.expected);
    if (!steps.length) {
      toast(t('err_case_steps'), 'error');
      return;
    }
    contentJson = JSON.stringify({
      preconditions: ed.preconditions,
      steps,
      test_data: ed.testData,
    });
  }
  const { ids: tagIds, unknown } = resolveTagIds(ed.tagNames);
  if (unknown.length) {
    toast(t('err_case_unknown_tags', { tags: unknown.join(', ') }), 'error');
    return;
  }
  try {
    const resp = await ApiBinary.one('projectStudioCaseSaveRequest', {
      projectId: projectId(),
      caseId: ed.caseId,
      kind: ed.kind,
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
    acknowledgeAttachmentUploads(projectId(), currentUserId(), ed.attachments);
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
  if (!canGenerateTests()) return;
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
        <div class="ps-field-hint">${escapeHtml(t('gen_kind_hint'))}</div>
        <div class="ps-choice-grid" data-gen-kinds>
          ${CASE_KINDS.map((k) => `
            <div class="ps-choice-card ${k === 'manual' ? 'is-selected' : ''}" data-gen-kind="${k}" role="button" tabindex="0">
              <div class="ps-cc-ico">${sprite(k === 'manual' ? 'list' : 'code')}</div>
              <div>
                <div class="ps-cc-name">${escapeHtml(t(`case_kind_${k}`))}</div>
                <div class="ps-cc-desc">${escapeHtml(t(`case_kind_${k}_desc`))}</div>
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
      <div class="ps-banner-info" data-gen-bound-agent hidden>${sprite('brain')}<span data-gen-bound-agent-text></span></div>
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

  // The kind decides which project agent binding runs the generation
  // (generation::agent_function_for_kind); the select below only overrides it.
  const renderBoundAgent = () => {
    const banner = body.querySelector('[data-gen-bound-agent]');
    const text = body.querySelector('[data-gen-bound-agent-text]');
    if (!banner || !text) return;
    const fn = KIND_AGENT_FUNCTION[gw.kind];
    if (!fn) { banner.hidden = true; return; }
    const bindings = Array.isArray(state.settings?.agents) ? state.settings.agents : [];
    const binding = bindings.find((a) => a.function === fn);
    const agentId = binding ? (fv(binding, 'agent_id') || '') : '';
    const known = state.agentOptions.find((a) => a.id === agentId);
    const name = known?.name || (binding ? fv(binding, 'agent_name') : '') || '';
    banner.hidden = false;
    text.textContent = name
      ? t('gen_agent_kind_bound', { fn: t(`agents_fn_${fn}`), agent: name })
      : t('gen_agent_kind_default', { fn: t(`agents_fn_${fn}`) });
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
    if (step === 3) { renderSummary(); renderBoundAgent(); }
    showError(null);
  };

  body.querySelector('[data-gen-kinds]')?.addEventListener('click', (e) => {
    const card = e.target.closest('[data-gen-kind]');
    if (!card || gw.busy) return;
    gw.kind = card.dataset.genKind;
    body.querySelectorAll('[data-gen-kind]').forEach((c) => {
      c.classList.toggle('is-selected', c.dataset.genKind === gw.kind);
    });
    showError(null);
  });

  const start = async () => {
    if (gw.busy || !canGenerateTests()) return;
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
    if (canArea('knowledge') && !state.sources.length) {
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
    if (!state.settings) {
      try {
        const resp = await ApiBinary.one('projectStudioSettingsGetRequest', { projectId: projectId() });
        state.settings = resp.settings;
      } catch { /* the per-kind banner falls back to the seeded agent */ }
    }
    if (gw.step === 3) renderBoundAgent();
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
      <tf-searchbox id="ps-gens-search" placeholder="${escapeAttr(t('gens_search_placeholder'))}" value="${escapeAttr(s.gensFilter || '')}"></tf-searchbox>
      <tf-select id="ps-gens-f-status" value="${escapeAttr(s.gensStatus || '')}">
        <option value="">${escapeHtml(t('gens_status_all'))}</option>
        ${['running', 'review', 'accepted', 'rejected', 'failed', 'cancelled'].map((x) => `<option value="${x}" ${x === s.gensStatus ? 'selected' : ''}>${escapeHtml(t(`gen_status_${x}`))}</option>`).join('')}
      </tf-select>
      <tf-select id="ps-gens-f-kind" value="${escapeAttr(s.gensKind || '')}">
        <option value="">${escapeHtml(t('gens_kind_all'))}</option>
        ${CASE_KINDS.map((k) => `<option value="${k}" ${k === s.gensKind ? 'selected' : ''}>${escapeHtml(t(`case_kind_${k}`))}</option>`).join('')}
      </tf-select>
      <span class="ps-toolbar-spacer"></span>
      <tf-button variant="ghost" icon="refresh" id="ps-gens-refresh">${escapeHtml(t('action_refresh'))}</tf-button>
      ${canArea('tests', 'write') ? `<tf-button variant="primary" icon="sparkle" id="ps-gens-new" ${canGenerateTests() ? '' : 'disabled'}>${escapeHtml(t('cases_generate'))}</tf-button>` : ''}
    </div>
    <div id="ps-gens-table-host"></div>
  `;
  byId('ps-gens-new')?.addEventListener('click', () => openGenerationWindow());
  byId('ps-gens-refresh')?.addEventListener('click', () => renderGenerationsView());
  byId('ps-gens-search')?.addEventListener('input', (e) => {
    s.gensFilter = String(e.detail?.value ?? e.target.value ?? '');
    renderGenerationsTable();
  });
  byId('ps-gens-f-status')?.addEventListener('change', (e) => {
    s.gensStatus = e.detail?.value ?? '';
    renderGenerationsTable();
  });
  byId('ps-gens-f-kind')?.addEventListener('change', (e) => {
    s.gensKind = e.detail?.value ?? '';
    renderGenerationsTable();
  });
  renderGenerationsTable();
}

function renderGenerationsTable() {
  const s = f2();
  const tableHost = byId('ps-gens-table-host');
  if (!tableHost) return;
  const needle = (s.gensFilter || '').trim().toLowerCase();
  const rows = s.gens.filter((g) => {
    if (s.gensStatus && g.status !== s.gensStatus) return false;
    if (s.gensKind && g.kind !== s.gensKind) return false;
    if (!needle) return true;
    return `${fv(g, 'agent_name') || ''} ${g.instructions || ''} ${fv(g, 'gen_id')}`.toLowerCase().includes(needle);
  });
  if (!rows.length) {
    tableHost.innerHTML = `<tf-empty-state icon="sparkle" title="${escapeAttr(s.gens.length ? t('gens_no_match') : t('gens_empty'))}"></tf-empty-state>`;
    return;
  }

  tableHost.innerHTML = `
    <tf-table id="ps-gens-table">
      <tf-column key="name" label="${escapeAttr(t('gens_col_name'))}" renderer="html"></tf-column>
      <tf-column key="kind" label="${escapeAttr(t('gens_col_kind'))}" renderer="chip"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('gens_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="agent" label="${escapeAttr(t('gens_col_agent'))}"></tf-column>
      <tf-column key="progress" label="${escapeAttr(t('gens_col_progress'))}"></tf-column>
      <tf-column key="startedBy" label="${escapeAttr(t('gens_col_started_by'))}"></tf-column>
      <tf-column key="startedAt" label="${escapeAttr(t('gens_col_started_at'))}"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-gens-table');
  table.rows = rows.map((g) => ({
    _id: fv(g, 'gen_id'),
    _row: g,
    name: `<div class="tf-table__cell-title">${escapeHtml((g.instructions || t('gens_untitled')).slice(0, 70))}</div>`
      + `<div class="tf-table__cell-sub">${escapeHtml(shortId(fv(g, 'gen_id')))}</div>`,
    kind: chipCell(CASE_KIND_CHIP[g.kind] || 'info', t(`case_kind_${g.kind}`)),
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
  table.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const wrap = document.createElement('div');
    wrap.className = 'ps-file-actions';
    const open = document.createElement('tf-button');
    open.setAttribute('variant', 'ghost');
    open.setAttribute('size', 'sm');
    open.setAttribute('icon', 'external-link');
    open.setAttribute('title', t('gens_open'));
    open.addEventListener('click', (e) => { e.stopPropagation(); openGenDetail(live()._id); });
    wrap.appendChild(open);
    const status = row._row.status;
    if (canArea('tests', 'admin') && status !== 'running' && status !== 'review') {
      const del = document.createElement('tf-button');
      del.setAttribute('variant', 'ghost');
      del.setAttribute('size', 'sm');
      del.setAttribute('icon', 'trash');
      del.setAttribute('title', t('action_delete'));
      del.addEventListener('click', (e) => { e.stopPropagation(); deleteGeneration(live()._id); });
      wrap.appendChild(del);
    }
    return wrap;
  };
  table.addEventListener('row-click', (e) => {
    const genId = e.detail?.row?._id;
    if (genId) openGenDetail(genId);
  });

  const running = s.gens.filter((g) => g.status === 'running').length;
  const review = s.gens.filter((g) => g.status === 'review').length;
  const footer = document.createElement('div');
  footer.className = 'ps-table-footer';
  footer.textContent = t('gens_footer', { shown: rows.length, total: s.gens.length, running, review });
  tableHost.appendChild(footer);
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
        ${running && canArea('tests', 'write') && (initiator || canArea('tests', 'admin')) ? `<tf-button variant="ghost" icon="ban" id="ps-gen-cancel">${escapeHtml(t('gen_cancel'))}</tf-button>` : ''}
        ${!running && run.status !== 'review' && canArea('tests', 'admin') ? `<tf-button variant="ghost" icon="trash" id="ps-gen-delete" title="${escapeAttr(t('action_delete'))}"></tf-button>` : ''}
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
          ${canArea('tests', 'write') ? `
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
// T12 — test environments (list, editor window, admin approval queue)
// =============================================================================

function envAuthLabel(env) {
  const auth = fv(env, 'auth_type') || 'none';
  const label = t(`env_auth_${auth}`);
  return fv(env, 'has_secret') ? `${label} · ${t('env_secret_stored')}` : label;
}

// Mirrors environments::classify_base_url closely enough to warn BEFORE the
// save round-trip; the server decision (which resolves DNS) always wins.
function looksPrivateUrl(raw) {
  let host = '';
  try {
    host = new URL(String(raw)).hostname;
  } catch {
    return false;
  }
  if (!host) return false;
  // A single-label host (no dot) never resolves publicly.
  if (!host.includes('.') && !host.includes(':')) return true;
  return PRIVATE_HOST_RE.test(host);
}

async function loadEnvironments(force = false) {
  const s = f2();
  if (s.envs.loaded && !force) return s.envs.rows;
  const resp = await ApiBinary.one('projectStudioEnvironmentsListRequest', { projectId: projectId() });
  s.envs.rows = Array.isArray(resp.environments) ? resp.environments : [];
  s.envs.loaded = true;
  s.envPending = s.envs.rows.filter((e) => fv(e, 'approval_status') === 'pending').length;
  return s.envs.rows;
}

function approvedEnvironments() {
  return canArea('environments') ? (f2().envs.rows || []).filter((e) => fv(e, 'approval_status') === 'approved') : [];
}

async function renderEnvironmentsView() {
  const host = byId('ps-tests-host');
  if (!host) return;
  const s = f2();
  try {
    await loadEnvironments(true);
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('envs_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || s.view !== 'environments') return;


  host.innerHTML = `
    <div class="ps-tests-toolbar">
      <tf-searchbox id="ps-envs-search" placeholder="${escapeAttr(t('envs_search_placeholder'))}" value="${escapeAttr(s.envs.filter || '')}"></tf-searchbox>
      <tf-select id="ps-envs-f-type" value="${escapeAttr(s.envs.type)}">
        ${selectOpt('', s.envs.type, t('env_filter_type_all'))}
        ${ENV_TYPES.map((x) => selectOpt(x, s.envs.type, t(`env_type_${x}`))).join('')}
      </tf-select>
      <tf-select id="ps-envs-f-status" value="${escapeAttr(s.envs.status)}">
        ${selectOpt('', s.envs.status, t('env_filter_status_all'))}
        ${['approved', 'pending', 'rejected'].map((x) => selectOpt(x, s.envs.status, t(`env_status_${x}`))).join('')}
      </tf-select>
      <span class="ps-toolbar-spacer"></span>
      ${state.isAdmin ? `
        <tf-button variant="ghost" icon="shield" id="ps-envs-approvals">
          ${escapeHtml(t('envs_approvals_btn'))}${s.envPending > 0 ? ` (${s.envPending})` : ''}
        </tf-button>
      ` : ''}
      ${canArea('environments', 'write') ? `<tf-button variant="primary" icon="plus" id="ps-envs-new">${escapeHtml(t('envs_new'))}</tf-button>` : ''}
    </div>
    <div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('envs_intro'))}</span></div>
    <div id="ps-envs-table-host"></div>
  `;

  byId('ps-envs-search')?.addEventListener('input', (e) => {
    s.envs.filter = String(e.detail?.value ?? e.target.value ?? '');
    renderEnvironmentsTable();
  });
  byId('ps-envs-f-type')?.addEventListener('change', (e) => {
    s.envs.type = e.detail?.value ?? e.target.value ?? '';
    renderEnvironmentsTable();
  });
  byId('ps-envs-f-status')?.addEventListener('change', (e) => {
    s.envs.status = e.detail?.value ?? e.target.value ?? '';
    renderEnvironmentsTable();
  });
  byId('ps-envs-new')?.addEventListener('click', () => openEnvironmentWindow(null));
  byId('ps-envs-approvals')?.addEventListener('click', () => {
    s.view = 'env-approvals';
    renderTestsView();
  });
  renderEnvironmentsTable();
}

function renderEnvironmentsTable() {
  const s = f2();
  const tableHost = byId('ps-envs-table-host');
  if (!tableHost) return;
  const needle = (s.envs.filter || '').trim().toLowerCase();
  const rows = s.envs.rows.filter((env) => {
    if (s.envs.type && fv(env, 'env_type') !== s.envs.type) return false;
    if (s.envs.status && fv(env, 'approval_status') !== s.envs.status) return false;
    if (needle && !`${env.name} ${fv(env, 'base_url') || ''}`.toLowerCase().includes(needle)) return false;
    return true;
  });
  if (!rows.length) {
    tableHost.innerHTML = `<tf-empty-state icon="globe" title="${escapeAttr(s.envs.rows.length ? t('envs_no_match') : t('envs_empty'))}"></tf-empty-state>`;
    return;
  }

  tableHost.innerHTML = `
    <tf-table id="ps-envs-table">
      <tf-column key="name" label="${escapeAttr(t('envs_col_name'))}"></tf-column>
      <tf-column key="type" label="${escapeAttr(t('envs_col_type'))}"></tf-column>
      <tf-column key="url" label="${escapeAttr(t('envs_col_url'))}" renderer="html"></tf-column>
      <tf-column key="auth" label="${escapeAttr(t('envs_col_auth'))}"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('envs_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="updated" label="${escapeAttr(t('envs_col_updated'))}"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-envs-table');
  table.rows = rows.map((env) => ({
    _id: fv(env, 'environment_id'),
    _row: env,
    name: env.name,
    type: t(`env_type_${fv(env, 'env_type')}`),
    url: `<span class="ps-mono">${escapeHtml(fv(env, 'base_url') || '')}</span>`,
    auth: envAuthLabel(env),
    status: chipCell(ENV_STATUS_CHIP[fv(env, 'approval_status')], t(`env_status_${fv(env, 'approval_status')}`)),
    updated: formatTimestamp(fv(env, 'updated_at')),
  }));
  table.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const wrap = document.createElement('div');
    wrap.className = 'ps-file-actions';
    if (!canArea('environments', 'write')) return wrap;
    const edit = document.createElement('tf-button');
    edit.setAttribute('variant', 'ghost');
    edit.setAttribute('size', 'sm');
    edit.setAttribute('icon', 'edit');
    edit.setAttribute('title', t('action_edit'));
    edit.addEventListener('click', (e) => { e.stopPropagation(); openEnvironmentWindow(live()._row); });
    wrap.appendChild(edit);
    const del = document.createElement('tf-button');
    del.setAttribute('variant', 'ghost');
    del.setAttribute('size', 'sm');
    del.setAttribute('icon', 'trash');
    del.setAttribute('title', t('action_delete'));
    del.addEventListener('click', (e) => { e.stopPropagation(); confirmDeleteEnvironment(live()._row); });
    wrap.appendChild(del);
    return wrap;
  };
  table.expandable = true;
  table.rowKey = '_id';
  table.expandRenderer = (row) => buildEnvExpansion(row._row);
}

// Expansion carries the details that would crowd the table: rejection reason,
// requester, extra headers and the sandbox host allowlist.
function buildEnvExpansion(env) {
  const wrap = document.createElement('div');
  wrap.className = 'ps-item-expansion';
  const allowlist = Array.isArray(fv(env, 'host_allowlist')) ? fv(env, 'host_allowlist') : [];
  const headers = fv(env, 'extra_headers_json') || '';
  const reason = fv(env, 'approval_reason') || '';
  wrap.innerHTML = `
    ${fv(env, 'is_private_address') ? `<div class="ps-banner-warn">${sprite('alert')}<span>${escapeHtml(t('env_private_address'))}</span></div>` : ''}
    ${reason ? `<div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('env_reason_prefix', { reason }))}</span></div>` : ''}
    <div class="ps-case-info-list">
      <div><span>${escapeHtml(t('env_requested_by'))}</span><b>${escapeHtml(fv(env, 'requested_by_name') || '—')}</b></div>
      <div><span>${escapeHtml(t('env_decided_by'))}</span><b>${escapeHtml(fv(env, 'decided_by_name') || '—')}</b></div>
      <div><span>${escapeHtml(t('env_decided_at'))}</span><b>${escapeHtml(fv(env, 'decided_at') ? formatTimestamp(fv(env, 'decided_at')) : '—')}</b></div>
      <div><span>${escapeHtml(t('env_allowlist_label'))}</span><b>${escapeHtml(allowlist.join(', ') || '—')}</b></div>
    </div>
    ${headers && headers !== '{}' ? `<div class="ps-code-block">${escapeHtml(headers)}</div>` : ''}
  `;
  return wrap;
}

// T12 window — create/edit. `secret` is input-only: left untouched it keeps the
// stored value (the wire sends null), emptied explicitly it clears it.
function openEnvironmentWindow(env) {
  const editing = !!env;
  const { body, foot, cleanup } = openWindow({
    title: t(editing ? 'env_win_edit_title' : 'env_win_new_title'),
    subtitle: editing ? env.name : t('env_win_sub'),
    icon: 'globe',
    width: 640,
  });

  const ew = {
    type: editing ? (fv(env, 'env_type') || 'web') : 'web',
    auth: editing ? (fv(env, 'auth_type') || 'none') : 'none',
    secretTouched: false,
    busy: false,
  };
  const allowlist = editing && Array.isArray(fv(env, 'host_allowlist')) ? fv(env, 'host_allowlist') : [];
  const headersJson = editing ? (fv(env, 'extra_headers_json') || '') : '';

  body.innerHTML = `
    <tf-input id="ps-env-name" label="${escapeAttr(t('env_name_label'))}" value="${escapeAttr(editing ? env.name : '')}"
      hint="${escapeAttr(t('env_name_hint'))}"></tf-input>
    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('env_type_label'))}</span>
      <tf-segmented id="ps-env-type" value="${escapeAttr(ew.type)}">
        ${ENV_TYPES.map((x) => `<option value="${x}">${escapeHtml(t(`env_type_${x}`))}</option>`).join('')}
      </tf-segmented>
      <div class="ps-field-hint">${escapeHtml(t('env_type_hint'))}</div>
    </div>
    <tf-input id="ps-env-url" label="${escapeAttr(t('env_url_label'))}" placeholder="https://staging.example.com"
      value="${escapeAttr(editing ? (fv(env, 'base_url') || '') : '')}" hint="${escapeAttr(t('env_url_hint'))}"></tf-input>
    <div class="ps-banner-warn" data-env-private hidden>${sprite('alert')}<span>${escapeHtml(t('env_private_banner'))}</span></div>
    <div class="ps-field" data-env-justification hidden>
      <tf-textarea id="ps-env-justification" rows="3" label="${escapeAttr(t('env_justification_label'))}"
        hint="${escapeAttr(t('env_justification_hint'))}"></tf-textarea>
    </div>
    <div class="ps-field">
      <tf-select id="ps-env-auth" label="${escapeAttr(t('env_auth_label'))}" value="${escapeAttr(ew.auth)}">
        ${ENV_AUTH_TYPES.map((x) => `<option value="${x}" ${x === ew.auth ? 'selected' : ''}>${escapeHtml(t(`env_auth_${x}`))}</option>`).join('')}
      </tf-select>
      <div class="ps-field-hint">${escapeHtml(t('env_auth_hint'))}</div>
    </div>
    <div class="ps-field" data-env-secret ${ew.auth === 'none' ? 'hidden' : ''}>
      <tf-input id="ps-env-secret" type="password" label="${escapeAttr(t('env_secret_label'))}"
        placeholder="${escapeAttr(editing && fv(env, 'has_secret') ? t('env_secret_keep_hint') : '')}"
        hint="${escapeAttr(t('env_secret_hint'))}"></tf-input>
    </div>
    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('env_allowlist_label'))}</span>
      <tf-tag-input id="ps-env-allowlist" dedupe placeholder="${escapeAttr(t('env_allowlist_placeholder'))}"></tf-tag-input>
      <div class="ps-field-hint">${escapeHtml(t('env_allowlist_hint'))}</div>
    </div>
    <div class="ps-field">
      <tf-textarea id="ps-env-headers" rows="3" label="${escapeAttr(t('env_headers_label'))}"
        placeholder='{"X-Tenant": "portal-b2b"}' hint="${escapeAttr(t('env_headers_hint'))}"></tf-textarea>
    </div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="primary" icon="check" data-action="save">${escapeHtml(t(editing ? 'action_save' : 'env_submit'))}</tf-button>
    </div>
  `;

  const tagInput = body.querySelector('#ps-env-allowlist');
  if (tagInput) tagInput.tags = allowlist.slice();
  const headersEl = body.querySelector('#ps-env-headers');
  if (headersEl && headersJson && headersJson !== '{}') headersEl.value = headersJson;
  const justificationEl = body.querySelector('#ps-env-justification');
  if (justificationEl && editing) justificationEl.value = fv(env, 'justification') || '';

  const showError = (msg) => {
    const el = body.querySelector('[data-form-error]');
    if (el) { el.hidden = !msg; el.textContent = msg || ''; }
  };
  const syncPrivate = () => {
    const url = String(body.querySelector('#ps-env-url')?.value ?? '').trim();
    const priv = looksPrivateUrl(url);
    const banner = body.querySelector('[data-env-private]');
    const justification = body.querySelector('[data-env-justification]');
    if (banner) banner.hidden = !priv;
    if (justification) justification.hidden = !priv;
    return priv;
  };

  body.querySelector('#ps-env-type')?.addEventListener('change', (e) => {
    ew.type = e.detail?.value ?? ew.type;
  });
  body.querySelector('#ps-env-url')?.addEventListener('input', () => syncPrivate());
  body.querySelector('#ps-env-auth')?.addEventListener('change', (e) => {
    ew.auth = e.detail?.value ?? e.target.value ?? 'none';
    const secretField = body.querySelector('[data-env-secret]');
    if (secretField) secretField.hidden = ew.auth === 'none';
  });
  body.querySelector('#ps-env-secret')?.addEventListener('input', () => { ew.secretTouched = true; });
  syncPrivate();

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || ew.busy) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }

    const name = String(body.querySelector('#ps-env-name')?.value ?? '').trim();
    const baseUrl = String(body.querySelector('#ps-env-url')?.value ?? '').trim();
    const justification = String(justificationEl?.value ?? '').trim();
    const headersRaw = String(headersEl?.value ?? '').trim();
    if (name.length < 2) { showError(t('err_env_name')); return; }
    if (!/^https?:\/\/.+/.test(baseUrl)) { showError(t('err_env_url')); return; }
    if (headersRaw) {
      try {
        const parsed = JSON.parse(headersRaw);
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('shape');
      } catch {
        showError(t('err_env_headers'));
        return;
      }
    }
    if (syncPrivate() && !justification) { showError(t('err_env_justification')); return; }

    // null = keep the stored secret, '' = clear it, value = replace it.
    let secret = null;
    if (ew.auth === 'none') secret = '';
    else if (ew.secretTouched) secret = String(body.querySelector('#ps-env-secret')?.value ?? '');
    else if (!editing) secret = String(body.querySelector('#ps-env-secret')?.value ?? '');

    showError(null);
    ew.busy = true;
    btn.setAttribute('disabled', '');
    try {
      const resp = await ApiBinary.one('projectStudioEnvironmentSaveRequest', {
        projectId: projectId(),
        environmentId: editing ? fv(env, 'environment_id') : null,
        name,
        envType: ew.type,
        baseUrl,
        authType: ew.auth,
        secret,
        extraHeadersJson: headersRaw,
        hostAllowlist: tagInput?.tags ?? [],
        justification,
      });
      const status = fv(resp, 'approval_status') || 'approved';
      toast(status === 'pending' ? t('env_save_pending') : t('env_saved'), 'success');
      cleanup();
      f2().envs.loaded = false;
      if (state.tab === 'tests' && f2().view === 'environments') await renderTestsView();
    } catch (err) {
      ew.busy = false;
      btn.removeAttribute('disabled');
      showError(`${t('env_save_failed')}: ${err.message}`);
    }
  });
}

function confirmDeleteEnvironment(env) {
  openDeleteWindow({
    title: t('env_delete_title'),
    targetName: env.name,
    targetSub: fv(env, 'base_url'),
    targetIcon: 'globe',
    warning: t('env_delete_warning'),
    confirmLabel: t('delete_forever'),
    onConfirm: async () => {
      await ApiBinary.one('projectStudioEnvironmentDeleteRequest', {
        projectId: projectId(),
        environmentId: fv(env, 'environment_id'),
      });
      toast(t('env_delete_ok'), 'success');
      f2().envs.loaded = false;
      await renderTestsView();
    },
  });
}

// Admin-only cross-project queue. Rendered as cards (not a table): each
// decision needs the requester's justification and a rejection reason field.
async function renderEnvApprovalsView() {
  const host = byId('ps-tests-host');
  if (!host) return;
  const s = f2();
  if (!state.isAdmin) {
    s.view = 'environments';
    return renderEnvironmentsView();
  }
  try {
    const resp = await ApiBinary.one('projectStudioEnvApprovalsListRequest', {});
    s.envApprovals.items = Array.isArray(resp.items) ? resp.items : [];
    s.envApprovals.loaded = true;
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('envs_approvals_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || s.view !== 'env-approvals') return;

  const items = s.envApprovals.items;
  host.innerHTML = `
    <div class="ps-editor-head">
      <tf-button variant="ghost" icon="chevron-left" id="ps-envs-back">${escapeHtml(t('envs_back'))}</tf-button>
      <div class="ps-editor-title-static">
        <div class="ps-detail-name">${escapeHtml(t('envs_approvals_title'))}</div>
        <div class="ps-detail-sub">${escapeHtml(t('envs_approvals_sub', { count: items.length }))}</div>
      </div>
    </div>
    <div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('envs_approvals_hint'))}</span></div>
    <div id="ps-env-approvals-list">
      ${items.length ? items.map((item) => {
        const env = item.environment || {};
        const envId = fv(env, 'environment_id');
        return `
          <div class="ps-approval-card" data-approval="${escapeAttr(envId)}" data-project="${escapeAttr(fv(item, 'project_id'))}">
            <div class="ps-approval-head">
              <div>
                <div class="ps-approval-name">${escapeHtml(env.name || '')}</div>
                <div class="ps-approval-url">${escapeHtml(fv(env, 'base_url') || '')}</div>
              </div>
              <div class="ps-approval-badges">
                <tf-chip status="info">${escapeHtml(t(`env_type_${fv(env, 'env_type')}`))}</tf-chip>
                <tf-status-pill status="warn" label="${escapeAttr(t('env_status_pending'))}"></tf-status-pill>
                ${fv(env, 'is_private_address') ? `<tf-chip status="warn">${escapeHtml(t('env_private_address'))}</tf-chip>` : ''}
              </div>
            </div>
            <div class="ps-approval-meta">
              <span>${escapeHtml(t('envs_approval_project'))}: <b>${escapeHtml(fv(item, 'project_name') || '')}</b></span>
              <span>${escapeHtml(t('env_requested_by'))}: <b>${escapeHtml(fv(env, 'requested_by_name') || '')}</b></span>
              <span>${escapeHtml(formatTimestamp(fv(env, 'created_at')))}</span>
              <span>${escapeHtml(t('env_auth_label'))}: <b>${escapeHtml(envAuthLabel(env))}</b></span>
            </div>
            <div class="ps-approval-justification">
              <span class="ps-field-label">${escapeHtml(t('env_justification_label'))}</span>
              <p>${escapeHtml(fv(item, 'justification') || '—')}</p>
            </div>
            <div class="ps-approval-actions">
              <tf-input data-reject-reason label="${escapeAttr(t('env_reject_reason_label'))}"
                placeholder="${escapeAttr(t('env_reject_reason_placeholder'))}"></tf-input>
              <tf-button variant="danger-solid" icon="ban" data-decide="reject">${escapeHtml(t('env_reject'))}</tf-button>
              <tf-button variant="primary" icon="check" data-decide="approve">${escapeHtml(t('env_approve'))}</tf-button>
            </div>
          </div>
        `;
      }).join('') : `<tf-empty-state icon="check" title="${escapeAttr(t('envs_approvals_empty'))}"></tf-empty-state>`}
    </div>
  `;

  byId('ps-envs-back')?.addEventListener('click', () => {
    s.view = 'environments';
    renderTestsView();
  });
  byId('ps-env-approvals-list')?.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-decide]');
    if (!btn) return;
    const card = btn.closest('[data-approval]');
    if (!card) return;
    const approve = btn.dataset.decide === 'approve';
    const reason = String(card.querySelector('[data-reject-reason]')?.value ?? '').trim();
    if (!approve && !reason) {
      toast(t('env_reject_reason_required'), 'error');
      return;
    }
    btn.setAttribute('disabled', '');
    try {
      await ApiBinary.one('projectStudioEnvApprovalDecideRequest', {
        projectId: card.dataset.project,
        environmentId: card.dataset.approval,
        approve,
        reason,
      });
      toast(t('env_decided_ok'), 'success');
      f2().envs.loaded = false;
      await renderTestsView();
    } catch (err) {
      btn.removeAttribute('disabled');
      toast(`${t('env_decide_failed')}: ${err.message}`, 'error');
    }
  });
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
      <tf-searchbox id="ps-suites-search" placeholder="${escapeAttr(t('suites_search_placeholder'))}" value="${escapeAttr(s.suitesFilter || '')}"></tf-searchbox>
      <tf-select id="ps-suites-health" value="${escapeAttr(s.suitesHealth || 'all')}">
        <option value="all">${escapeHtml(t('suites_health_all'))}</option>
        <option value="ok">${escapeHtml(t('suites_health_ok'))}</option>
        <option value="deprecated">${escapeHtml(t('suites_health_deprecated'))}</option>
      </tf-select>
      <span class="ps-toolbar-spacer"></span>
      ${canArea('tests', 'write') ? `<tf-button variant="primary" icon="plus" id="ps-suites-new">${escapeHtml(t('suites_new'))}</tf-button>` : ''}
    </div>
    <div id="ps-suites-table-host"></div>
  `;
  byId('ps-suites-new')?.addEventListener('click', () => openSuiteEditor(null));
  byId('ps-suites-search')?.addEventListener('input', (e) => {
    s.suitesFilter = String(e.detail?.value ?? e.target.value ?? '');
    renderSuitesTable();
  });
  byId('ps-suites-health')?.addEventListener('change', (e) => {
    s.suitesHealth = e.detail?.value || 'all';
    renderSuitesTable();
  });
  renderSuitesTable();
}

function visibleSuites() {
  const s = f2();
  const needle = (s.suitesFilter || '').trim().toLowerCase();
  const health = s.suitesHealth || 'all';
  return s.suites.filter((suite) => {
    if (needle && !`${suite.name} ${fv(suite, 'suite_id') || ''}`.toLowerCase().includes(needle)) return false;
    if (health === 'ok' && fv(suite, 'has_deprecated')) return false;
    if (health === 'deprecated' && !fv(suite, 'has_deprecated')) return false;
    return true;
  });
}

function renderSuitesTable() {
  const s = f2();
  const rows = visibleSuites();
  const tableHost = byId('ps-suites-table-host');
  if (!tableHost) return;
  if (!rows.length) {
    tableHost.innerHTML = `<tf-empty-state icon="grid-rows" title="${escapeAttr(s.suites.length ? t('suites_no_match') : t('suites_empty'))}"></tf-empty-state>`;
    return;
  }

  tableHost.innerHTML = `
    <tf-table id="ps-suites-table">
      <tf-column key="name" label="${escapeAttr(t('suites_col_name'))}" renderer="html"></tf-column>
      <tf-column key="cases" label="${escapeAttr(t('suites_col_cases'))}" renderer="num"></tf-column>
      <tf-column key="deprecated" label="${escapeAttr(t('suites_col_health'))}" renderer="chip"></tf-column>
      <tf-column key="lastRun" label="${escapeAttr(t('suites_col_last_run'))}"></tf-column>
      <tf-column key="updated" label="${escapeAttr(t('suites_col_updated'))}"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-suites-table');
  table.rows = rows.map((suite) => {
    const lastRun = fv(suite, 'last_run');
    return {
      _id: fv(suite, 'suite_id'),
      _row: suite,
      name: `<div class="tf-table__cell-title">${escapeHtml(suite.name)}</div>`
        + `<div class="tf-table__cell-sub">${escapeHtml(shortId(fv(suite, 'suite_id')))}${suite.description ? ` · ${escapeHtml(suite.description)}` : ''}</div>`,
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
  table.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
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
    if (canArea('tests', 'write')) {
      mk('edit', t('action_edit'), () => openSuiteEditor(live()._id));
      mk('play', t('suites_run'), () => openRunWindow({ suiteId: live()._id }));
      mk('trash', t('action_delete'), () => deleteSuite(live()._row));
    } else {
      mk('eye', t('suites_open'), () => openSuiteEditor(live()._id));
    }
    return wrap;
  };
  table.addEventListener('row-click', (e) => {
    const suiteId = e.detail?.row?._id;
    if (suiteId) openSuiteEditor(suiteId);
  });

  const assignments = rows.reduce((sum, suite) => sum + Number(fv(suite, 'case_count') ?? 0), 0);
  const footer = document.createElement('div');
  footer.className = 'ps-table-footer';
  footer.textContent = t('suites_footer', { shown: rows.length, total: s.suites.length, assignments });
  tableHost.appendChild(footer);
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
    poolKind: 'all',
    poolSelected: new Set(),
    memberFilter: '',
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
  const editable = canArea('tests', 'write');

  host.innerHTML = `
    <div class="ps-suite-head">
      <div class="ps-suite-head-row">
        <tf-button variant="ghost" icon="chevron-left" id="ps-suite-back">${escapeHtml(t('back_to_suites'))}</tf-button>
        <div class="ps-suite-head-fields">
          <tf-input id="ps-suite-name" label="${escapeAttr(t('suite_name_label'))}" value="${escapeAttr(se.name)}" ${editable ? '' : 'readonly'}></tf-input>
          <tf-input id="ps-suite-desc" label="${escapeAttr(t('suite_desc_label'))}" value="${escapeAttr(se.description)}" ${editable ? '' : 'readonly'}></tf-input>
        </div>
        ${editable ? `
          <div class="ps-suite-head-actions">
            <tf-button variant="ghost" icon="play" id="ps-suite-save-run">${escapeHtml(t('suite_save_and_run'))}</tf-button>
            <tf-button variant="primary" icon="check" id="ps-suite-save">${escapeHtml(t('suite_save'))}</tf-button>
          </div>
        ` : ''}
      </div>
      <div class="ps-suite-summary" id="ps-suite-summary"></div>
    </div>
    <div class="ps-two-pane">
      <tf-section-card title="${escapeAttr(t('suite_pool_title'))}" icon="list">
        <span slot="subtitle">${escapeHtml(t('suite_pool_sub'))}</span>
        ${editable ? `<tf-button slot="actions" variant="ghost" size="sm" icon="plus" id="ps-suite-add-selected" disabled>${escapeHtml(t('suite_add_selected', { count: 0 }))}</tf-button>` : ''}
        <div class="ps-pane-toolbar">
          <tf-searchbox id="ps-suite-pool-filter" placeholder="${escapeAttr(t('suite_pool_filter'))}" debounce="200" value="${escapeAttr(se.poolFilter)}"></tf-searchbox>
          <tf-select id="ps-suite-pool-kind" value="${escapeAttr(se.poolKind)}">
            <option value="all">${escapeHtml(t('suite_kind_all'))}</option>
            ${CASE_KINDS.map((k) => `<option value="${k}">${escapeHtml(t(`case_kind_${k}`))}</option>`).join('')}
          </tf-select>
        </div>
        <div class="ps-pane-list" id="ps-suite-pool"></div>
      </tf-section-card>
      <tf-section-card title="${escapeAttr(t('suite_members_title'))}" icon="grid-rows">
        <span slot="subtitle">${escapeHtml(t('suite_members_sub', { count: se.cases.length }))}</span>
        ${editable ? `<tf-button slot="actions" variant="ghost" size="sm" icon="x" id="ps-suite-clear">${escapeHtml(t('suite_clear'))}</tf-button>` : ''}
        <div class="ps-pane-toolbar">
          <tf-searchbox id="ps-suite-member-filter" placeholder="${escapeAttr(t('suite_member_filter'))}" debounce="200" value="${escapeAttr(se.memberFilter)}"></tf-searchbox>
        </div>
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
  byId('ps-suite-pool-filter')?.addEventListener('input', (e) => {
    se.poolFilter = String(e.detail?.value ?? e.target.value ?? '');
    renderSuitePool(editable);
  });
  byId('ps-suite-pool-kind')?.addEventListener('change', (e) => {
    se.poolKind = e.detail?.value || 'all';
    renderSuitePool(editable);
  });
  byId('ps-suite-member-filter')?.addEventListener('input', (e) => {
    se.memberFilter = String(e.detail?.value ?? e.target.value ?? '');
    renderSuiteMembers(editable);
  });
  byId('ps-suite-add-selected')?.addEventListener('click', () => {
    for (const c of se.pool) {
      const id = fv(c, 'case_id');
      if (!se.poolSelected.has(id)) continue;
      se.cases.push({ caseId: id, title: c.title, priority: c.priority, status: c.status, kind: c.kind });
    }
    se.poolSelected.clear();
    renderSuitePool(editable);
    renderSuiteMembers(editable);
  });
  byId('ps-suite-clear')?.addEventListener('click', () => {
    se.cases = [];
    renderSuitePool(editable);
    renderSuiteMembers(editable);
  });

  const save = async () => {
    const name = se.name.trim();
    if (name.length < 2) { toast(t('err_suite_name'), 'error'); return null; }
    const resp = await ApiBinary.one('projectStudioSuiteSaveRequest', {
      projectId: projectId(),
      suiteId: se.suiteId,
      name,
      description: se.description.trim(),
      caseIds: se.cases.map((c) => c.caseId),
    });
    toast(t('suite_saved'), 'success');
    return fv(resp, 'suite_id') || se.suiteId;
  };
  byId('ps-suite-save')?.addEventListener('click', async () => {
    try {
      if (!(await save())) return;
      f2().suiteEditor = null;
      f2().view = 'suites';
      await renderTestsView();
    } catch (err) {
      toast(`${t('suite_save_failed')}: ${err.message}`, 'error');
    }
  });
  byId('ps-suite-save-run')?.addEventListener('click', async () => {
    try {
      const suiteId = await save();
      if (!suiteId) return;
      f2().suiteEditor = null;
      f2().view = 'suites';
      await renderTestsView();
      openRunWindow({ suiteId });
    } catch (err) {
      toast(`${t('suite_save_failed')}: ${err.message}`, 'error');
    }
  });

  renderSuitePool(editable);
  renderSuiteMembers(editable);
}

// Header chips mirror the mockup summary line: size, per-kind breakdown and
// whether the suite still drags deprecated cases along.
function renderSuiteSummary() {
  const se = f2().suiteEditor;
  const host = byId('ps-suite-summary');
  if (!host || !se) return;
  const counts = new Map();
  let deprecated = 0;
  for (const c of se.cases) {
    const kind = c.kind || 'manual';
    counts.set(kind, (counts.get(kind) || 0) + 1);
    if (c.status === 'deprecated') deprecated += 1;
  }
  const kindChips = [...counts.entries()]
    .map(([kind, n]) => `<tf-chip status="${CASE_KIND_CHIP[kind] || 'info'}">${n} ${escapeHtml(t(`case_kind_${kind}`))}</tf-chip>`)
    .join('');
  host.innerHTML = `
    <tf-chip status="accent">${escapeHtml(t('suite_cases_chip', { count: se.cases.length }))}</tf-chip>
    ${kindChips}
    ${deprecated
      ? `<tf-chip status="warn">${escapeHtml(t('suite_deprecated_chip', { count: deprecated }))}</tf-chip>`
      : `<tf-chip status="ok">${escapeHtml(t('suite_all_approved'))}</tf-chip>`}
  `;
}

function renderSuitePool(editable) {
  const se = f2().suiteEditor;
  const host = byId('ps-suite-pool');
  if (!host || !se) return;
  const chosen = new Set(se.cases.map((c) => c.caseId));
  const query = se.poolFilter.trim().toLowerCase();
  const rows = se.pool.filter((c) => {
    const id = fv(c, 'case_id');
    if (chosen.has(id)) return false;
    if (se.poolKind !== 'all' && (c.kind || 'manual') !== se.poolKind) return false;
    if (query && !`${c.title} ${id}`.toLowerCase().includes(query)) return false;
    return true;
  });
  // Selections of now-hidden rows would silently add invisible cases on
  // "add selected", so drop them whenever the filter changes.
  const visible = new Set(rows.map((c) => fv(c, 'case_id')));
  for (const id of [...se.poolSelected]) if (!visible.has(id)) se.poolSelected.delete(id);
  syncSuiteAddSelected();

  const counter = byId('ps-suite-pool-count');
  if (counter) counter.textContent = String(rows.length);

  if (!rows.length) {
    host.innerHTML = `<tf-empty-state icon="list" title="${escapeAttr(t('suite_pool_empty'))}"></tf-empty-state>`;
    return;
  }
  host.innerHTML = rows.map((c) => {
    const id = fv(c, 'case_id');
    return `
    <div class="ps-pane-row">
      ${editable ? `<tf-checkbox data-pool-pick="${escapeAttr(id)}" ${se.poolSelected.has(id) ? 'checked' : ''}></tf-checkbox>` : ''}
      <div class="ps-pane-row-main">
        <div class="ps-pane-row-title">${escapeHtml(c.title)}</div>
        <div class="ps-pane-row-meta">
          <span class="ps-pane-row-id">${escapeHtml(shortId(id))}</span>
          <tf-chip status="${CASE_KIND_CHIP[c.kind] || 'info'}">${escapeHtml(t(`case_kind_${c.kind || 'manual'}`))}</tf-chip>
          <tf-chip status="${PRIORITY_CHIP[c.priority] || 'info'}">${escapeHtml(t(`prio_${c.priority}`))}</tf-chip>
        </div>
      </div>
      ${editable ? `<tf-button variant="ghost" size="sm" icon="plus" data-pool-add="${escapeAttr(id)}" title="${escapeAttr(t('suite_add_case'))}"></tf-button>` : ''}
    </div>
  `;
  }).join('');
  host.querySelectorAll('[data-pool-pick]').forEach((box) => {
    box.addEventListener('change', (e) => {
      const id = box.dataset.poolPick;
      if (e.detail?.checked ?? box.checked) se.poolSelected.add(id);
      else se.poolSelected.delete(id);
      syncSuiteAddSelected();
    });
  });
  host.querySelectorAll('[data-pool-add]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const c = se.pool.find((x) => fv(x, 'case_id') === btn.dataset.poolAdd);
      if (!c) return;
      se.cases.push({ caseId: fv(c, 'case_id'), title: c.title, priority: c.priority, status: c.status, kind: c.kind });
      se.poolSelected.delete(btn.dataset.poolAdd);
      renderSuitePool(editable);
      renderSuiteMembers(editable);
    });
  });
}

function syncSuiteAddSelected() {
  const se = f2().suiteEditor;
  const btn = byId('ps-suite-add-selected');
  if (!btn || !se) return;
  const n = se.poolSelected.size;
  btn.textContent = t('suite_add_selected', { count: n });
  if (n) btn.removeAttribute('disabled');
  else btn.setAttribute('disabled', '');
}

function renderSuiteMembers(editable) {
  const se = f2().suiteEditor;
  const host = byId('ps-suite-members');
  if (!host || !se) return;
  renderSuiteSummary();
  const sub = byId('ps-suite-members')?.closest('tf-section-card')?.querySelector('[slot="subtitle"]');
  if (sub) sub.textContent = t('suite_members_sub', { count: se.cases.length });
  if (!se.cases.length) {
    host.innerHTML = `<tf-empty-state icon="grid-rows" title="${escapeAttr(t('suite_members_empty'))}"></tf-empty-state>`;
    return;
  }
  const query = (se.memberFilter || '').trim().toLowerCase();
  const shown = se.cases
    .map((c, i) => ({ c, i }))
    .filter(({ c }) => !query || `${c.title} ${c.caseId}`.toLowerCase().includes(query));
  if (!shown.length) {
    host.innerHTML = `<tf-empty-state icon="search" title="${escapeAttr(t('suite_members_no_match'))}"></tf-empty-state>`;
    return;
  }
  host.innerHTML = shown.map(({ c, i }) => `
    <div class="ps-pane-row">
      <div class="ps-step-num">${i + 1}</div>
      <div class="ps-pane-row-main">
        <div class="ps-pane-row-title">${escapeHtml(c.title)}</div>
        <div class="ps-pane-row-meta">
          <span class="ps-pane-row-id">${escapeHtml(shortId(c.caseId))}</span>
          <tf-chip status="${CASE_KIND_CHIP[c.kind] || 'info'}">${escapeHtml(t(`case_kind_${c.kind || 'manual'}`))}</tf-chip>
          ${c.status === 'deprecated' ? `<tf-chip status="warn">${escapeHtml(t('case_status_deprecated'))}</tf-chip>` : ''}
        </div>
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
      <tf-searchbox id="ps-runs-search" placeholder="${escapeAttr(t('runs_search_placeholder'))}" value="${escapeAttr(s.runs.filter || '')}"></tf-searchbox>
      <tf-select id="ps-runs-f-type" value="${escapeAttr(s.runs.type || '')}">
        <option value="">${escapeHtml(t('runs_type_all'))}</option>
        ${['manual', 'auto', 'perf'].map((x) => `<option value="${x}" ${x === s.runs.type ? 'selected' : ''}>${escapeHtml(t(`run_type_${x}`))}</option>`).join('')}
      </tf-select>
      <tf-select id="ps-runs-f-status" value="${escapeAttr(s.runs.status)}">
        <option value="" ${s.runs.status === '' ? 'selected' : ''}>${escapeHtml(t('runs_filter_all'))}</option>
        ${['running', 'completed', 'cancelled', 'error'].map((x) => `<option value="${x}" ${x === s.runs.status ? 'selected' : ''}>${escapeHtml(t(`run_status_${x}`))}</option>`).join('')}
      </tf-select>
      <span class="ps-toolbar-spacer"></span>
      <tf-button variant="ghost" icon="refresh" id="ps-runs-refresh">${escapeHtml(t('action_refresh'))}</tf-button>
      ${canArea('tests', 'write') ? `<tf-button variant="primary" icon="plus" id="ps-runs-new">${escapeHtml(t('runs_new'))}</tf-button>` : ''}
    </div>
    <div id="ps-runs-table-host"></div>
  `;
  byId('ps-runs-search')?.addEventListener('input', (e) => {
    s.runs.filter = String(e.detail?.value ?? e.target.value ?? '');
    renderRunsTable();
  });
  byId('ps-runs-f-type')?.addEventListener('change', (e) => {
    s.runs.type = e.detail?.value ?? '';
    renderRunsTable();
  });
  byId('ps-runs-f-status')?.addEventListener('change', (e) => {
    s.runs.status = e.detail?.value ?? e.target.value ?? '';
    s.runs.page = 1;
    renderRunsView();
  });
  byId('ps-runs-refresh')?.addEventListener('click', () => renderRunsView());
  byId('ps-runs-new')?.addEventListener('click', () => openRunWindow({}));
  renderRunsTable();
}

function visibleRuns() {
  const s = f2();
  const needle = (s.runs.filter || '').trim().toLowerCase();
  return s.runs.rows.filter((run) => {
    if (s.runs.type && (fv(run, 'run_type') || 'manual') !== s.runs.type) return false;
    if (!needle) return true;
    return `${run.name} ${fv(run, 'suite_name') || ''} #${fv(run, 'run_no')}`.toLowerCase().includes(needle);
  });
}

function renderRunsTable() {
  const s = f2();
  const tableHost = byId('ps-runs-table-host');
  if (!tableHost) return;
  const rows = visibleRuns();
  if (!rows.length) {
    tableHost.innerHTML = `<tf-empty-state icon="play" title="${escapeAttr(s.runs.rows.length ? t('runs_no_match') : t('runs_empty'))}"></tf-empty-state>`;
    return;
  }

  tableHost.innerHTML = `
    <tf-table id="ps-runs-table" page-size="${F2_PAGE_SIZE}" total="${s.runs.total}" page="${s.runs.page}">
      <tf-column key="name" label="${escapeAttr(t('runs_col_name'))}" renderer="html"></tf-column>
      <tf-column key="suite" label="${escapeAttr(t('runs_col_suite'))}"></tf-column>
      <tf-column key="type" label="${escapeAttr(t('runs_col_type'))}" renderer="chip"></tf-column>
      <tf-column key="environment" label="${escapeAttr(t('runs_col_env'))}"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('runs_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="progress" label="${escapeAttr(t('runs_col_progress'))}" renderer="html"></tf-column>
      <tf-column key="result" label="${escapeAttr(t('runs_col_result'))}"></tf-column>
      <tf-column key="createdBy" label="${escapeAttr(t('runs_col_created_by'))}"></tf-column>
      <tf-column key="startedAt" label="${escapeAttr(t('runs_col_started'))}"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-runs-table');
  const assignRows = () => {
    table.rows = visibleRuns().map((run) => {
      const total = Number(run.total ?? 0);
      const done = Number(run.passed ?? 0) + Number(run.failed ?? 0) + Number(run.blocked ?? 0)
        + Number(run.skipped ?? 0) + Number(run.errored ?? 0);
      const pct = total ? Math.round((done / total) * 100) : 0;
      const tone = { completed: 'success', cancelled: 'warning', error: 'danger' }[run.status] || 'accent';
      return {
        _id: fv(run, 'run_id'),
        _row: run,
        name: `<div class="tf-table__cell-title">${escapeHtml(run.name)}</div><div class="tf-table__cell-sub">#${fv(run, 'run_no')}</div>`,
        suite: fv(run, 'suite_name') || t('runs_adhoc'),
        type: chipCell(fv(run, 'run_type') === 'manual' ? 'neutral' : 'accent', t(`run_type_${fv(run, 'run_type') || 'manual'}`)),
        environment: fv(run, 'environment_name') || t(`assign_${fv(run, 'assignment_mode') || 'pool'}`),
        status: chipCell(RUN_STATUS_CHIP[run.status], t(`run_status_${run.status}`)),
        progress: '<div style="display:flex;align-items:center;gap:8px;min-width:120px;">'
          + `<tf-progress-bar value="${pct}" tone="${tone}" size="sm" style="flex:1 1 80px;"></tf-progress-bar>`
          + `<span style="font-size:11px;opacity:0.7;">${pct}%</span></div>`,
        result: t('runs_result', { done, total, passed: Number(run.passed ?? 0), failed: Number(run.failed ?? 0) }),
        createdBy: fv(run, 'created_by_name') || '',
        startedAt: formatTimestamp(fv(run, 'started_at')),
      };
    });
  };
  assignRows();
  table.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const wrap = document.createElement('div');
    wrap.className = 'ps-file-actions';
    const open = document.createElement('tf-button');
    open.setAttribute('variant', 'ghost');
    open.setAttribute('size', 'sm');
    open.setAttribute('icon', 'external-link');
    open.setAttribute('title', t('runs_open'));
    open.addEventListener('click', (e) => {
      e.stopPropagation();
      const r = live();
      openRunByType(r._id, fv(r._row, 'run_type'));
    });
    wrap.appendChild(open);
    if (canArea('tests', 'admin') && row._row.status !== 'running') {
      const del = document.createElement('tf-button');
      del.setAttribute('variant', 'ghost');
      del.setAttribute('size', 'sm');
      del.setAttribute('icon', 'trash');
      del.setAttribute('title', t('action_delete'));
      del.addEventListener('click', (e) => { e.stopPropagation(); deleteRun(live()._row); });
      wrap.appendChild(del);
    }
    return wrap;
  };
  table.addEventListener('row-click', (e) => {
    const row = e.detail?.row;
    if (row?._id) openRunByType(row._id, fv(row._row, 'run_type'));
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

  const running = s.runs.rows.filter((r) => r.status === 'running').length;
  const footer = document.createElement('div');
  footer.className = 'ps-table-footer';
  footer.textContent = t('runs_footer', { shown: rows.length, total: s.runs.total, running });
  tableHost.appendChild(footer);
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
  if (!canArea('tests', 'write') || (prefill.runType && prefill.runType !== 'manual' && !canTryRun())) return;
  const { body, foot, cleanup } = openWindow({
    title: t('run_win_title'),
    subtitle: t('run_win_sub'),
    icon: 'play',
    width: 720,
  });

  const rw = {
    name: prefill.name || '',
    // 'manual' keeps the F2 tester flow; 'auto'/'perf' submit to the test
    // runner (RunStartAuto) — the server derives the final run_type from the
    // selected case kinds, the choice here drives the form.
    runType: prefill.runType || 'manual',
    source: prefill.fromFailedRunId ? 'failed' : 'suite',
    suiteId: prefill.suiteId || '',
    caseIds: new Set(),
    fromFailedRunId: prefill.fromFailedRunId || '',
    envNote: '',
    environmentId: '',
    runnerServiceId: '',
    perf: { ...PERF_DEFAULT_PROFILE },
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
      <tf-input id="ps-run-name" label="${escapeAttr(t('run_name_label'))}" placeholder="${escapeAttr(t('run_name_placeholder'))}" value="${escapeAttr(rw.name)}"></tf-input>
    </div>
    <div class="ps-field" style="margin-bottom:12px;">
      <span class="ps-field-label">${escapeHtml(t('run_type_label'))}</span>
      <tf-segmented id="ps-run-type" value="${escapeAttr(rw.runType)}"></tf-segmented>
      <div class="ps-field-hint" data-run-type-hint>${escapeHtml(t('run_type_manual_hint'))}</div>
    </div>
    <div class="ps-banner-warn" data-runner-missing hidden>${sprite('alert')}<span>${escapeHtml(t('run_runner_missing'))}</span></div>
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
    <div class="ps-field" style="margin:12px 0;" data-run-mode-form="manual">
      <tf-input id="ps-run-env" label="${escapeAttr(t('run_env_label'))}" placeholder="${escapeAttr(t('run_env_placeholder'))}"></tf-input>
    </div>
    <div data-run-mode-form="automated" hidden>
      <div class="ps-field" style="margin:12px 0;">
        <tf-select id="ps-run-environment" label="${escapeAttr(t('run_env_select_label'))}" value=""></tf-select>
        <div class="ps-field-hint">${escapeHtml(t('run_env_select_hint'))}</div>
      </div>
      <div class="ps-field" style="margin-bottom:12px;">
        <tf-select id="ps-run-runner" label="${escapeAttr(t('run_runner_label'))}" value=""></tf-select>
        <div class="ps-field-hint" data-runner-hint>${escapeHtml(t('run_runner_hint'))}</div>
      </div>
      <div class="ps-field" data-perf-form hidden>
        <span class="ps-field-label">${escapeHtml(t('run_perf_label'))}</span>
        <div class="ps-perf-form">
          <tf-input id="ps-run-perf-users" type="number" min="${PERF_LIMITS.users[0]}" max="${PERF_LIMITS.users[1]}"
            label="${escapeAttr(t('run_perf_users_label'))}" value="${rw.perf.users}"></tf-input>
          <tf-input id="ps-run-perf-spawn" type="number" min="${PERF_LIMITS.spawnRate[0]}" max="${PERF_LIMITS.spawnRate[1]}"
            label="${escapeAttr(t('run_perf_spawn_label'))}" value="${rw.perf.spawn_rate}"></tf-input>
          <tf-input id="ps-run-perf-duration" type="number" min="${PERF_LIMITS.duration[0]}" max="${PERF_LIMITS.duration[1]}"
            label="${escapeAttr(t('run_perf_duration_label'))}" value="${rw.perf.duration_secs}"></tf-input>
        </div>
        <div class="ps-field-hint">${escapeHtml(t('run_perf_hint'))}</div>
      </div>
    </div>
    <div class="ps-field" style="margin-bottom:12px;" data-run-mode-form="manual">
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
  body.querySelector('#ps-run-type').setOptions(RUN_TYPES.map((value) => ({ value, label: t(`run_type_${value}`), disabled: value !== 'manual' && !canTryRun() })), rw.runType);
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
  const isAutomated = () => rw.runType !== 'manual';

  const syncRunType = () => {
    const automated = isAutomated();
    body.querySelectorAll('[data-run-mode-form]').forEach((form) => {
      const wantsAutomated = form.dataset.runModeForm === 'automated';
      form.hidden = wantsAutomated !== automated;
    });
    const perfForm = body.querySelector('[data-perf-form]');
    if (perfForm) perfForm.hidden = rw.runType !== 'perf';
    const hint = body.querySelector('[data-run-type-hint]');
    if (hint) hint.textContent = t(`run_type_${rw.runType}_hint`);
    const runnerBanner = body.querySelector('[data-runner-missing]');
    if (runnerBanner) runnerBanner.hidden = !automated || !!(f2().runners || []).length;
    const createBtn = foot.querySelector('[data-action="create"]');
    if (createBtn) createBtn.setAttribute('label', t(automated ? 'run_start_auto' : 'run_create'));
    syncAssignForms();
  };

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
      form.hidden = isAutomated() || form.dataset.runAssignForm !== rw.mode;
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

  body.querySelector('#ps-run-type')?.addEventListener('change', (e) => {
    if (e.detail?.value !== 'manual' && !canTryRun()) {
      e.target.setAttribute('value', rw.runType);
      return;
    }
    rw.runType = e.detail?.value ?? rw.runType;
    showError(null);
    syncRunType();
  });
  body.querySelector('#ps-run-environment')?.addEventListener('change', (e) => {
    rw.environmentId = e.detail?.value ?? '';
  });
  body.querySelector('#ps-run-runner')?.addEventListener('change', (e) => {
    rw.runnerServiceId = e.detail?.value ?? '';
    const runner = (f2().runners || []).find((r) => fv(r, 'service_id') === rw.runnerServiceId);
    const hint = body.querySelector('[data-runner-hint]');
    if (hint) hint.textContent = runner ? runnerToolchainLabel(runner) : t('run_runner_hint');
  });
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
    if (!canArea('tests', 'write') || (isAutomated() && !canTryRun())) return;
    rw.name = String(body.querySelector('#ps-run-name')?.value ?? '').trim();
    rw.envNote = String(body.querySelector('#ps-run-env')?.value ?? '').trim();
    if (rw.name.length < 3) { showError(t('err_run_name')); return; }
    if (rw.source === 'suite' && !rw.suiteId) { showError(t('err_run_suite')); return; }
    if (rw.source === 'cases' && !rw.caseIds.size) { showError(t('err_run_cases')); return; }
    if (rw.source === 'failed' && !rw.fromFailedRunId) { showError(t('err_run_failed')); return; }

    if (isAutomated()) {
      if (!rw.environmentId) { showError(t('err_run_env')); return; }
      let perfProfileJson = '';
      if (rw.runType === 'perf') {
        const users = Number(body.querySelector('#ps-run-perf-users')?.value ?? 0);
        const spawnRate = Number(body.querySelector('#ps-run-perf-spawn')?.value ?? 0);
        const duration = Number(body.querySelector('#ps-run-perf-duration')?.value ?? 0);
        const inRange = (value, [min, max]) => Number.isFinite(value) && value >= min && value <= max;
        if (!inRange(users, PERF_LIMITS.users) || !inRange(spawnRate, PERF_LIMITS.spawnRate)
          || !inRange(duration, PERF_LIMITS.duration)) {
          showError(t('err_run_perf'));
          return;
        }
        perfProfileJson = JSON.stringify({ users, spawn_rate: spawnRate, duration_secs: duration });
      }
      showError(null);
      rw.busy = true;
      btn.setAttribute('disabled', '');
      try {
        const resp = await ApiBinary.one('projectStudioRunStartAutoRequest', {
          projectId: projectId(),
          name: rw.name,
          suiteId: rw.source === 'suite' ? rw.suiteId : '',
          caseIds: rw.source === 'cases' ? [...rw.caseIds] : [],
          fromRunId: rw.source === 'failed' ? rw.fromFailedRunId : '',
          environmentId: rw.environmentId,
          runnerServiceId: rw.runnerServiceId,
          perfProfileJson,
        });
        toast(t('run_auto_started', { no: Number(fv(resp, 'run_no') ?? 0) }), 'success');
        cleanup();
        const runId = fv(resp, 'run_id');
        if (runId) await openAutoRun(runId);
      } catch (err) {
        rw.busy = false;
        btn.removeAttribute('disabled');
        showError(`${t('run_auto_start_failed')}: ${err.message}`);
      }
      return;
    }

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

  syncRunType();

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

    // Automated runs need an APPROVED environment and (optionally) an explicit
    // runner; an empty runner lets the server match one by toolchain.
    if (canArea('environments')) {
      try {
        await loadEnvironments();
      } catch { /* the select stays empty and the guard below blocks the start */ }
    }
    const approved = approvedEnvironments();
    rw.environmentId = approved.length === 1 ? fv(approved[0], 'environment_id') : '';
    body.querySelector('#ps-run-environment')?.setOptions([
      { value: '', label: approved.length ? t('assign_choose') : t('run_env_none') },
      ...approved.map((env) => ({ value: fv(env, 'environment_id'), label: `${env.name} — ${fv(env, 'base_url')}` })),
    ], rw.environmentId);
    await loadRunners();
    body.querySelector('#ps-run-runner')?.setOptions([
      { value: '', label: t('run_runner_auto') },
      ...(f2().runners || []).map((r) => ({
        value: fv(r, 'service_id'),
        label: `${fv(r, 'display_name') || fv(r, 'engine_id')} — ${runnerToolchainLabel(r)}`,
      })),
    ], '');

    syncSourceForms();
    syncRunType();
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

// Manual runs open the tester-facing detail (T11 manual), automated/perf runs
// open the live/results view (T10/T11 auto). An unknown type (deep link from a
// notification) is resolved with one RunGet before the branch.
async function openRunByType(runId, runType) {
  let type = runType;
  if (!type) {
    try {
      const resp = await ApiBinary.one('projectStudioRunGetRequest', { projectId: projectId(), runId });
      type = fv(resp.run ?? {}, 'run_type') || 'manual';
    } catch {
      type = 'manual';
    }
  }
  if (type === 'auto' || type === 'perf') return openAutoRun(runId);
  return openRunDetail(runId);
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
  const runTotal = Number(run.total ?? 0);
  const runPassRate = runTotal ? Math.round((Number(run.passed ?? 0) / runTotal) * 1000) / 10 : 0;
  const runShare = (n) => (runTotal ? Math.round((Number(n ?? 0) / runTotal) * 100) : 0);
  const openItems = Number(run.pending ?? 0) + Number(fv(run, 'in_progress') ?? 0);
  const runDuration = runElapsed(run);
  const creator = isMe(fv(run, 'created_by'));
  const canClose = running && canArea('tests', 'write') && (canArea('tests', 'admin') || creator);
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
        ${canArea('tests', 'write') && failedCount > 0 && !running ? `<tf-button variant="ghost" icon="rotate" id="ps-run-from-failed">${escapeHtml(t('run_new_from_failed'))}</tf-button>` : ''}
        ${canClose ? `<tf-button variant="ghost" icon="check" id="ps-run-close">${escapeHtml(t('run_close'))}</tf-button>` : ''}
        ${canClose ? `<tf-button variant="ghost" icon="ban" id="ps-run-cancel">${escapeHtml(t('run_cancel'))}</tf-button>` : ''}
        ${canArea('tests', 'admin') && !running ? `<tf-button variant="ghost" icon="trash" id="ps-run-delete" title="${escapeAttr(t('action_delete'))}"></tf-button>` : ''}
      </div>
    </div>

    <div class="ps-kpi-grid ps-run-kpis">
      <tf-stat-card icon="check" accent="success" label="${escapeAttr(t('item_status_passed'))}"
        value="${Number(run.passed ?? 0)}" suffix="/${runTotal}"
        delta="${escapeAttr(t('run_kpi_pass_rate', { rate: runPassRate }))}" delta-type="neutral"></tf-stat-card>
      <tf-stat-card icon="x" accent="${Number(run.failed ?? 0) ? 'danger' : 'success'}" label="${escapeAttr(t('item_status_failed'))}"
        value="${Number(run.failed ?? 0)}"
        delta="${escapeAttr(t('run_kpi_share', { pct: runShare(run.failed) }))}" delta-type="${Number(run.failed ?? 0) ? 'warn' : 'neutral'}"></tf-stat-card>
      <tf-stat-card icon="ban" accent="warning" label="${escapeAttr(t('item_status_blocked'))}"
        value="${Number(run.blocked ?? 0)}"
        delta="${escapeAttr(t('run_kpi_share', { pct: runShare(run.blocked) }))}" delta-type="neutral"></tf-stat-card>
      <tf-stat-card icon="chevron-right" label="${escapeAttr(t('item_status_skipped'))}"
        value="${Number(run.skipped ?? 0)}"
        delta="${escapeAttr(t('run_kpi_share', { pct: runShare(run.skipped) }))}" delta-type="neutral"></tf-stat-card>
      <tf-stat-card icon="clock" accent="info" label="${escapeAttr(t('run_kpi_duration'))}"
        value="${escapeAttr(runDuration)}"
        delta="${escapeAttr(t('run_kpi_open_delta', { count: openItems }))}" delta-type="neutral"></tf-stat-card>
    </div>
    <div class="ps-run-progressbar" id="ps-run-stacked"></div>

    ${canArea('tests', 'write') && running ? `
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
      <span slot="actions">
        <tf-select id="ps-run-items-filter" value="${escapeAttr(rd.itemFilter || '')}">
          <option value="">${escapeHtml(t('run_items_filter_all'))}</option>
          ${['passed', 'failed', 'blocked', 'skipped', 'pending', 'in_progress'].map((x) => `<option value="${x}">${escapeHtml(t(`item_status_${x}`))}</option>`).join('')}
        </tf-select>
      </span>
      <div id="ps-run-items-host"></div>
    </tf-section-card>
  `;

  byId('ps-run-items-filter')?.addEventListener('change', (e) => {
    rd.itemFilter = e.detail?.value ?? '';
    renderRunItemsTable(rd);
  });

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
  const rows = rd.itemFilter ? rd.items.filter((it) => it.status === rd.itemFilter) : rd.items;
  if (!rows.length) {
    hostEl.innerHTML = `<tf-empty-state icon="list" title="${escapeAttr(rd.items.length ? t('run_items_no_match') : t('run_items_empty'))}"></tf-empty-state>`;
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
  table.rows = rows.map((it) => ({
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
      ...itemAtts.map((att) => ({ att, owner: attachmentOwner('run_item', fv(item, 'item_id')) })),
      ...steps.flatMap((step) => (Array.isArray(step.attachments) ? step.attachments : []).map((att) => ({ att, owner: attachmentOwner('run_step', fv(item, 'item_id'), fv(step, 'step_index')) }))),
    ];
    const attHost = wrap.querySelector('[data-exp-atts]');
    if (attHost && allAtts.length) {
      for (const { att, owner } of allAtts) {
        const mime = fv(att, 'mime') || '';
        const cell = document.createElement('div');
        cell.className = 'ps-exp-att';
        cell.title = fv(att, 'name') || '';
        if (mime.startsWith('image/')) {
          const img = document.createElement('img');
          img.alt = fv(att, 'name') || '';
          cell.appendChild(img);
          openAttachmentStream(owner, att).then((stream) => {
            if (img.isConnected || cell.isConnected) {
              img.src = stream.url;
              const observer = new MutationObserver(() => { if (!cell.isConnected) { stream.close(); observer.disconnect(); } });
              observer.observe(document.body, { childList: true, subtree: true });
            } else stream.close();
          }).catch(() => { cell.textContent = fv(att, 'name') || ''; });
        } else {
          cell.textContent = fv(att, 'name') || '';
        }
        cell.addEventListener('click', () => openAttachmentPreview(att, owner));
        attHost.appendChild(cell);
      }
    }
  })();
  return wrap;
}

// SQLite timestamps carry no zone marker but are UTC, so they must be parsed
// the same way formatTimestamp does — otherwise a fresh run looks hours long.
function parseTimestamp(value) {
  if (!value) return NaN;
  const raw = String(value);
  return Date.parse(raw.includes('T') ? raw : `${raw.replace(' ', 'T')}Z`);
}

// Wall-clock length of a run; a still-running run measures against "now".
function runElapsed(run) {
  const started = parseTimestamp(fv(run, 'started_at'));
  if (!Number.isFinite(started)) return '—';
  const finishedRaw = fv(run, 'finished_at');
  const end = finishedRaw ? parseTimestamp(finishedRaw) : Date.now();
  if (!Number.isFinite(end) || end < started) return '—';
  return formatDuration(Math.round((end - started) / 1000));
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
// T10/T11 — automated + perf runs (live view, artifacts, perf metrics)
// =============================================================================

// Test-runner discovery is shared by the run window and the code editor; the
// list is small and changes rarely, so one fetch per opened project is enough.
async function loadRunners(force = false) {
  const s = f2();
  if (s.runners && !force) return s.runners;
  try {
    const resp = await ApiBinary.one('projectStudioRunnersListRequest', { projectId: projectId() });
    s.runners = Array.isArray(resp.runners) ? resp.runners : [];
  } catch {
    s.runners = [];
  }
  return s.runners;
}

function runnerLanguages() {
  return new Set((f2().runners || []).flatMap((r) => (Array.isArray(r.toolchains) ? r.toolchains : []))
    .map((tc) => String(tc.language || '').toLowerCase()));
}

function runnerToolchainLabel(runner) {
  const chains = Array.isArray(runner.toolchains) ? runner.toolchains : [];
  if (!chains.length) return t('runner_no_toolchains');
  return chains
    .map((tc) => `${tc.language}${Array.isArray(tc.frameworks) && tc.frameworks.length ? ` (${tc.frameworks.join(', ')})` : ''}`)
    .join(' · ');
}

function openAutoRun(runId) {
  stopTestsLive();
  f2().autoRun = {
    runId,
    run: null,
    items: [],
    perfStats: [],
    perfTimeline: [],
    log: [],
    watchdog: '',
    unsub: null,
    pollTimer: null,
    autoScroll: true,
  };
  f2().view = 'auto-run';
  return renderTestsView();
}

function stopAutoRunLive() {
  const ar = state.f2?.autoRun;
  if (!ar) return;
  if (ar.unsub) { try { ar.unsub(); } catch { /* stream already gone */ } ar.unsub = null; }
  if (ar.pollTimer) { clearInterval(ar.pollTimer); ar.pollTimer = null; }
}

async function loadAutoRun(ar) {
  const resp = await ApiBinary.one('projectStudioRunAutoGetRequest', {
    projectId: projectId(), runId: ar.runId,
  });
  ar.run = resp.run || null;
  ar.items = Array.isArray(resp.items) ? resp.items : [];
  ar.perfStats = Array.isArray(fv(resp, 'perf_stats')) ? fv(resp, 'perf_stats') : [];
  ar.perfTimeline = Array.isArray(fv(resp, 'perf_timeline')) ? fv(resp, 'perf_timeline') : [];
}

async function renderAutoRunView() {
  const host = byId('ps-tests-host');
  const s = f2();
  const ar = s.autoRun;
  if (!host || !ar) { s.view = 'runs'; return renderRunsView(); }
  try {
    await loadAutoRun(ar);
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('auto_load_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || s.view !== 'auto-run') return;
  const run = ar.run;
  if (!run) { s.view = 'runs'; return renderRunsView(); }

  const running = run.status === 'running';
  const isPerf = fv(run, 'run_type') === 'perf';
  const total = Math.max(1, Number(run.total ?? ar.items.length ?? 1));
  const done = Number(run.passed ?? 0) + Number(run.failed ?? 0) + Number(run.blocked ?? 0)
    + Number(run.skipped ?? 0) + Number(run.errored ?? 0);
  const pct = Math.min(100, Math.round((done / total) * 100));
  const creator = isMe(fv(run, 'created_by'));
  const canCancel = running && canArea('tests', 'write') && (canArea('tests', 'admin') || creator);

  host.innerHTML = `
    <div class="ps-editor-head">
      <tf-button variant="ghost" icon="chevron-left" id="ps-auto-back">${escapeHtml(t('back_to_runs'))}</tf-button>
      <div class="ps-editor-title-static">
        <div class="ps-detail-name">#${fv(run, 'run_no')} ${escapeHtml(run.name)}</div>
        <div class="ps-detail-sub">
          ${escapeHtml(fv(run, 'suite_name') || t('runs_adhoc'))} ·
          ${escapeHtml(t('auto_env_prefix'))}: ${escapeHtml(fv(run, 'environment_name') || '—')} ·
          ${escapeHtml(fv(run, 'created_by_name') || '')} · ${escapeHtml(formatTimestamp(fv(run, 'started_at')))}
        </div>
      </div>
      <div class="ps-editor-badges">
        <tf-chip status="${RUN_STATUS_CHIP[run.status] || 'info'}" dot>${escapeHtml(t(`run_status_${run.status}`))}</tf-chip>
        <tf-chip status="info">${escapeHtml(t(`run_type_${fv(run, 'run_type') || 'auto'}`))}</tf-chip>
      </div>
      <div class="ps-editor-actions">
        <tf-button variant="ghost" icon="refresh" id="ps-auto-refresh">${escapeHtml(t('refresh'))}</tf-button>
        <tf-button variant="ghost" icon="download" id="ps-auto-export">${escapeHtml(t('run_export_csv'))}</tf-button>
        ${canArea('tests', 'write') && !running ? `<tf-button variant="ghost" icon="rotate" id="ps-auto-again">${escapeHtml(t('auto_run_again'))}</tf-button>` : ''}
        ${canCancel ? `<tf-button variant="danger-solid" icon="stop" id="ps-auto-stop">${escapeHtml(t('auto_stop'))}</tf-button>` : ''}
      </div>
    </div>

    <div class="ps-banner-warn" id="ps-auto-watchdog" ${ar.watchdog ? '' : 'hidden'}>
      ${sprite('alert')}<span id="ps-auto-watchdog-text">${escapeHtml(ar.watchdog ? t('auto_watchdog_banner', { detail: ar.watchdog }) : '')}</span>
    </div>

    <div class="ps-auto-progress">
      <tf-progress-bar id="ps-auto-progressbar" value="${pct}"></tf-progress-bar>
      <span class="ps-auto-progress-label" id="ps-auto-progress-label">${escapeHtml(t('auto_progress', { done, total: Number(run.total ?? ar.items.length ?? 0), pct }))}</span>
    </div>

    <div class="ps-kpi-grid ps-run-kpis">
      <tf-stat-card size="sm" icon="check" accent="success" label="${escapeAttr(t('item_status_passed'))}" value="${Number(run.passed ?? 0)}"></tf-stat-card>
      <tf-stat-card size="sm" icon="x" accent="danger" label="${escapeAttr(t('item_status_failed'))}" value="${Number(run.failed ?? 0)}"></tf-stat-card>
      <tf-stat-card size="sm" icon="alert" accent="danger" label="${escapeAttr(t('item_status_error'))}" value="${Number(run.errored ?? 0)}"></tf-stat-card>
      <tf-stat-card size="sm" icon="chevron-right" label="${escapeAttr(t('item_status_skipped'))}" value="${Number(run.skipped ?? 0)}"></tf-stat-card>
      <tf-stat-card size="sm" icon="clock" label="${escapeAttr(t('run_kpi_open'))}" value="${Number(run.pending ?? 0) + Number(fv(run, 'in_progress') ?? 0)}"></tf-stat-card>
    </div>

    ${isPerf ? `
      <div id="ps-perf-host"></div>
    ` : ''}

    <tf-section-card title="${escapeAttr(t('auto_items_title'))}" icon="list">
      <span slot="subtitle">${escapeHtml(t('run_items_sub', { count: ar.items.length }))}</span>
      <div id="ps-auto-items-host"></div>
    </tf-section-card>

    <tf-section-card title="${escapeAttr(t('auto_log_title'))}" icon="prompt">
      <span slot="actions">
        <label class="ps-toggle-field">
          <span class="ps-field-label">${escapeHtml(t('auto_log_autoscroll'))}</span>
          <tf-toggle id="ps-auto-autoscroll" ${ar.autoScroll ? 'checked' : ''}></tf-toggle>
        </label>
        <tf-button variant="ghost" size="sm" icon="download" id="ps-auto-log-download">${escapeHtml(t('auto_log_download'))}</tf-button>
      </span>
      <div class="ps-live-log" id="ps-auto-log">${escapeHtml(ar.log.join('\n'))}</div>
    </tf-section-card>
  `;

  byId('ps-auto-back')?.addEventListener('click', () => {
    stopAutoRunLive();
    s.autoRun = null;
    s.view = 'runs';
    renderTestsView();
  });
  byId('ps-auto-refresh')?.addEventListener('click', () => renderTestsView());
  byId('ps-auto-export')?.addEventListener('click', () => exportAutoRunCsv(ar));
  byId('ps-auto-again')?.addEventListener('click', () => openRunWindow({
    runType: isPerf ? 'perf' : 'auto',
    suiteId: fv(run, 'suite_id') || '',
    name: run.name,
  }));
  byId('ps-auto-stop')?.addEventListener('click', () => cancelAutoRun(ar.runId));
  byId('ps-auto-autoscroll')?.addEventListener('change', (e) => {
    ar.autoScroll = !!(e.detail?.checked ?? e.target.checked);
  });
  byId('ps-auto-log-download')?.addEventListener('click', () => {
    downloadTextFile(`run-${fv(run, 'run_no')}.log`, ar.log.join('\n'), 'text/plain');
  });

  renderAutoItemsTable(ar);
  if (isPerf) renderPerfSection(ar);
  const logEl = byId('ps-auto-log');
  if (logEl && ar.autoScroll) logEl.scrollTop = logEl.scrollHeight;

  if (running) startAutoRunLive(ar);
}

function renderAutoItemsTable(ar) {
  const hostEl = byId('ps-auto-items-host');
  if (!hostEl) return;
  if (!ar.items.length) {
    hostEl.innerHTML = `<tf-empty-state icon="list" title="${escapeAttr(t('auto_no_items'))}"></tf-empty-state>`;
    return;
  }
  hostEl.innerHTML = `
    <tf-table id="ps-auto-items-table">
      <tf-column key="pos" label="#" renderer="num"></tf-column>
      <tf-column key="title" label="${escapeAttr(t('auto_items_col_case'))}"></tf-column>
      <tf-column key="kind" label="${escapeAttr(t('auto_items_col_kind'))}"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('auto_items_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="duration" label="${escapeAttr(t('auto_items_col_duration'))}"></tf-column>
      <tf-column key="message" label="${escapeAttr(t('auto_items_col_message'))}"></tf-column>
      <tf-column key="artifacts" label="${escapeAttr(t('auto_items_col_artifacts'))}" renderer="num"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-auto-items-table');
  table.rowKey = '_id';
  table.expandable = true;
  table.expandRenderer = (row) => buildAutoItemExpansion(row._row);
  assignAutoItemRows(table, ar);
}

function assignAutoItemRows(table, ar) {
  table.rows = ar.items.map((it) => ({
    _id: fv(it, 'item_id'),
    _row: it,
    pos: Number(it.position ?? 0) + 1,
    title: fv(it, 'case_title'),
    kind: t(`case_kind_${it.kind}`),
    status: chipCell(ITEM_STATUS_CHIP[it.status], t(`item_status_${it.status}`)),
    duration: formatMillis(Number(fv(it, 'duration_ms') ?? 0)),
    message: String(it.message || '').slice(0, 160),
    artifacts: Array.isArray(fv(it, 'artifact_refs')) ? fv(it, 'artifact_refs').length : 0,
  }));
}

// T11 expansion: full failure message + artifact buttons (images preview
// inline, everything else downloads through the binary protocol).
function buildAutoItemExpansion(item) {
  const wrap = document.createElement('div');
  wrap.className = 'ps-item-expansion';
  const artifacts = Array.isArray(fv(item, 'artifact_refs')) ? fv(item, 'artifact_refs') : [];
  wrap.innerHTML = `
    ${item.message ? `<div class="ps-code-block">${escapeHtml(item.message)}</div>` : `<div class="ps-field-hint">${escapeHtml(t('auto_item_no_message'))}</div>`}
    <div class="ps-artifact-row">
      ${!canArea('tests') ? `<span class="ps-field-hint">${escapeHtml(t('access_read_only'))}</span>`
        : artifacts.length ? artifacts.map((art, i) => `
        <tf-button variant="ghost" size="sm" icon="${ARTIFACT_ICON[fv(art, 'kind')] || 'paperclip'}" data-artifact="${i}">
          ${escapeHtml(fv(art, 'name') || t(`artifact_kind_${fv(art, 'kind')}`))} · ${escapeHtml(formatBytes(Number(fv(art, 'size_bytes') ?? 0)))}
        </tf-button>
      `).join('') : `<span class="ps-field-hint">${escapeHtml(t('auto_item_no_artifacts'))}</span>`}
    </div>
  `;
  wrap.querySelectorAll('[data-artifact]').forEach((btn) => {
    btn.addEventListener('click', () => openArtifact(artifacts[Number(btn.dataset.artifact)]));
  });
  return wrap;
}

// Artifacts travel as CBOR bytes (no signed URL on the dashboard tier): images
// open in a preview window, everything else downloads from an object URL.
async function openArtifact(artifact) {
  if (!artifact) return;
  let resp = null;
  try {
    resp = await ApiBinary.one('projectStudioRunArtifactGetRequest', {
      projectId: projectId(),
      artifactId: fv(artifact, 'artifact_id'),
      maxBytes: ARTIFACT_MAX_BYTES,
    });
  } catch (err) {
    toast(`${t('artifact_download_failed')}: ${err.message}`, 'error');
    return;
  }
  const bytes = resp.bytes instanceof Uint8Array ? resp.bytes : new Uint8Array(resp.bytes || []);
  const mime = resp.mime || fv(artifact, 'mime') || 'application/octet-stream';
  const name = fv(artifact, 'name') || 'artifact';
  const url = URL.createObjectURL(new Blob([bytes], { type: mime }));
  if (resp.truncated) toast(t('artifact_truncated'), 'info');
  if (mime.startsWith('image/')) {
    const { body, foot, cleanup } = openWindow({ title: name, subtitle: mime, icon: 'image', width: 900 });
    body.innerHTML = `<div class="ps-att-preview"><img alt="${escapeAttr(name)}"></div>`;
    body.querySelector('img').src = url;
    foot.innerHTML = `
      <div class="ps-footer-left"></div>
      <div class="ps-footer-right">
        <tf-button variant="ghost" icon="download" data-action="download">${escapeHtml(t('attachment_download'))}</tf-button>
        <tf-button variant="ghost" data-action="close-artifact">${escapeHtml(t('action_close'))}</tf-button>
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

// T10 perf metrics + T11 perf results: the same section serves both, the live
// timeline simply grows while the run is running.
function renderPerfSection(ar) {
  const hostEl = byId('ps-perf-host');
  if (!hostEl) return;
  const timeline = ar.perfTimeline;
  const stats = ar.perfStats;
  const last = timeline[timeline.length - 1] || {};
  const totals = stats.reduce((acc, row) => {
    acc.requests += Number(row.requests ?? 0);
    acc.failures += Number(row.failures ?? 0);
    acc.rps += Number(row.rps ?? 0);
    acc.p90 = Math.max(acc.p90, Number(fv(row, 'p90_ms') ?? 0));
    acc.p99 = Math.max(acc.p99, Number(fv(row, 'p99_ms') ?? 0));
    return acc;
  }, { requests: 0, failures: 0, rps: 0, p90: 0, p99: 0 });
  const rps = Number(last.rps ?? totals.rps ?? 0);
  const p90 = Number(fv(last, 'p90_ms') ?? totals.p90 ?? 0);
  const errPct = totals.requests > 0 ? (totals.failures / totals.requests) * 100 : 0;

  if (!timeline.length && !stats.length) {
    hostEl.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('perf_empty'))}</div>`;
    return;
  }

  hostEl.innerHTML = `
    <div class="ps-kpi-grid ps-perf-kpis">
      <tf-stat-card size="sm" icon="bolt" label="${escapeAttr(t('perf_kpi_rps'))}" value="${formatNum(rps)}" suffix="req/s"></tf-stat-card>
      <tf-stat-card size="sm" icon="clock" label="${escapeAttr(t('perf_kpi_p90'))}" value="${formatNum(p90)}" suffix="ms"></tf-stat-card>
      <tf-stat-card size="sm" icon="clock" label="${escapeAttr(t('perf_kpi_p99'))}" value="${formatNum(totals.p99)}" suffix="ms"></tf-stat-card>
      <tf-stat-card size="sm" icon="alert" accent="${errPct > 1 ? 'danger' : 'info'}" label="${escapeAttr(t('perf_kpi_errors'))}" value="${formatNum(errPct)}" suffix="%"></tf-stat-card>
    </div>
    <tf-section-card title="${escapeAttr(t('perf_timeline_title'))}" icon="chart-line">
      <div id="ps-perf-chart" class="ps-perf-chart"></div>
    </tf-section-card>
    <tf-section-card title="${escapeAttr(t('perf_endpoints_title'))}" icon="network">
      <span slot="actions">
        <tf-button variant="ghost" size="sm" icon="download" id="ps-perf-export">${escapeHtml(t('perf_export_csv'))}</tf-button>
      </span>
      <div id="ps-perf-table-host"></div>
    </tf-section-card>
  `;

  const chartHost = byId('ps-perf-chart');
  if (chartHost && timeline.length) {
    const chart = document.createElement('tf-line-chart');
    chart.height = 220;
    chart.xAxis = { scale: 'category', min: null, max: null, ticks: null, format: null };
    chart.yAxis = { scale: 'linear', min: 0, max: null, ticks: 5, format: null };
    chart.series = [
      {
        id: 'rps',
        name: t('perf_series_rps'),
        tone: 'info',
        style: 'solid',
        showInLegend: true,
        points: timeline.map((p) => ({ x: formatElapsed(Number(fv(p, 'ts_s') ?? 0)), y: Number(p.rps ?? 0) })),
      },
      {
        id: 'p90',
        name: t('perf_series_p90'),
        tone: 'warning',
        style: 'dashed',
        showInLegend: true,
        points: timeline.map((p) => ({ x: formatElapsed(Number(fv(p, 'ts_s') ?? 0)), y: Number(fv(p, 'p90_ms') ?? 0) })),
      },
    ];
    chartHost.replaceChildren(chart);
  } else if (chartHost) {
    chartHost.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('perf_empty'))}</div>`;
  }

  const tableHost = byId('ps-perf-table-host');
  if (tableHost) {
    if (!stats.length) {
      tableHost.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('perf_empty'))}</div>`;
    } else {
      tableHost.innerHTML = `
        <tf-table id="ps-perf-table">
          <tf-column key="endpoint" label="${escapeAttr(t('perf_col_endpoint'))}"></tf-column>
          <tf-column key="requests" label="${escapeAttr(t('perf_col_requests'))}" renderer="num"></tf-column>
          <tf-column key="rps" label="${escapeAttr(t('perf_col_rps'))}" renderer="num"></tf-column>
          <tf-column key="p50" label="${escapeAttr(t('perf_col_p50'))}" renderer="num"></tf-column>
          <tf-column key="p90" label="${escapeAttr(t('perf_col_p90'))}" renderer="num"></tf-column>
          <tf-column key="p99" label="${escapeAttr(t('perf_col_p99'))}" renderer="num"></tf-column>
          <tf-column key="failures" label="${escapeAttr(t('perf_col_failures'))}" renderer="num"></tf-column>
        </tf-table>
      `;
      byId('ps-perf-table').rows = stats.map((row) => ({
        endpoint: row.endpoint,
        requests: Number(row.requests ?? 0),
        rps: formatNum(Number(row.rps ?? 0)),
        p50: formatNum(Number(fv(row, 'p50_ms') ?? 0)),
        p90: formatNum(Number(fv(row, 'p90_ms') ?? 0)),
        p99: formatNum(Number(fv(row, 'p99_ms') ?? 0)),
        failures: Number(row.failures ?? 0),
      }));
    }
    byId('ps-perf-export')?.addEventListener('click', () => {
      if (!stats.length) { toast(t('reports_no_data'), 'info'); return; }
      downloadTextFile(`perf-${fv(ar.run, 'run_no')}.csv`, toCsv(stats.map((row) => ({
        endpoint: row.endpoint,
        requests: Number(row.requests ?? 0),
        failures: Number(row.failures ?? 0),
        rps: Number(row.rps ?? 0),
        p50_ms: Number(fv(row, 'p50_ms') ?? 0),
        p90_ms: Number(fv(row, 'p90_ms') ?? 0),
        p99_ms: Number(fv(row, 'p99_ms') ?? 0),
        avg_ms: Number(fv(row, 'avg_ms') ?? 0),
      }))));
    });
  }
}

// Live view: the stream feeds the log/items incrementally, the 3 s
// RunAutoGet poll stays the source of truth (same contract as ingest).
function startAutoRunLive(ar) {
  const s = f2();
  stopAutoRunLive();

  const appendLog = (line) => {
    ar.log.push(line);
    if (ar.log.length > AUTO_LOG_CAP) ar.log.splice(0, ar.log.length - AUTO_LOG_CAP);
    const el = byId('ps-auto-log');
    if (el) {
      el.textContent = ar.log.join('\n');
      if (ar.autoScroll) el.scrollTop = el.scrollHeight;
    }
  };

  ApiBinary.subscribe(
    'projectStudioRunAutoStreamRequest',
    { projectId: projectId(), runId: ar.runId },
    {
      onChunk: (body) => {
        if (body?.variant !== 'ProjectStudioRunAutoStreamChunk') return;
        if (s.autoRun !== ar || s.view !== 'auto-run') return;
        const kind = body.kind;
        if (kind === 'log' || kind === 'phase') {
          appendLog(body.phase ? `[${body.phase}] ${body.line}` : String(body.line || ''));
          if (body.phase === 'watchdog') showAutoWatchdog(ar, String(body.line || ''));
        } else if (kind === 'item' && body.item) {
          const incoming = body.item;
          const idx = ar.items.findIndex((it) => fv(it, 'item_id') === fv(incoming, 'item_id'));
          if (idx >= 0) ar.items[idx] = incoming;
          else ar.items.push(incoming);
          const table = byId('ps-auto-items-table');
          if (table) assignAutoItemRows(table, ar);
          appendLog(`${fv(incoming, 'case_title')} — ${t(`item_status_${incoming.status}`)}`);
        } else if (kind === 'artifact' && body.artifact) {
          appendLog(`${t('auto_artifact_ready')}: ${fv(body.artifact, 'name')}`);
        }
      },
      onError: () => { /* the poll below is the source of truth */ },
      onEnd: () => { /* terminal state is confirmed by the poll */ },
    },
  ).then((unsub) => {
    if (s.autoRun !== ar || s.view !== 'auto-run') { unsub(); return; }
    ar.unsub = unsub;
  }).catch(() => { /* stream is optional; polling still tracks the run */ });

  ar.pollTimer = setInterval(async () => {
    if (s.autoRun !== ar || s.view !== 'auto-run' || state.tab !== 'tests') {
      stopAutoRunLive();
      return;
    }
    try {
      await loadAutoRun(ar);
    } catch {
      return;
    }
    const run = ar.run;
    if (!run) return;
    const total = Math.max(1, Number(run.total ?? 1));
    const done = Number(run.passed ?? 0) + Number(run.failed ?? 0) + Number(run.blocked ?? 0)
      + Number(run.skipped ?? 0) + Number(run.errored ?? 0);
    const pct = Math.min(100, Math.round((done / total) * 100));
    byId('ps-auto-progressbar')?.setAttribute('value', String(pct));
    const label = byId('ps-auto-progress-label');
    if (label) label.textContent = t('auto_progress', { done, total: Number(run.total ?? 0), pct });
    const table = byId('ps-auto-items-table');
    if (table) assignAutoItemRows(table, ar);
    if (fv(run, 'run_type') === 'perf') renderPerfSection(ar);
    if (run.status !== 'running') {
      stopAutoRunLive();
      await renderTestsView();
    }
  }, AUTO_RUN_POLL_MS);
}

function showAutoWatchdog(ar, line) {
  ar.watchdog = line || t('auto_watchdog_banner');
  const banner = byId('ps-auto-watchdog');
  const text = byId('ps-auto-watchdog-text');
  if (text) text.textContent = t('auto_watchdog_banner', { detail: ar.watchdog });
  if (banner) banner.hidden = false;
}

async function cancelAutoRun(runId) {
  const ok = await TfWindow.confirm({
    title: t('auto_cancel_title'),
    message: t('auto_cancel_message'),
    confirmLabel: t('auto_stop'),
    cancelLabel: t('action_cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await ApiBinary.one('projectStudioRunAutoCancelRequest', { projectId: projectId(), runId });
    toast(t('auto_cancelled_ok'), 'success');
    await renderTestsView();
  } catch (err) {
    toast(`${t('auto_cancel_failed')}: ${err.message}`, 'error');
  }
}

function exportAutoRunCsv(ar) {
  const rows = ar.items.map((it) => ({
    position: Number(it.position ?? 0) + 1,
    case_title: fv(it, 'case_title'),
    kind: it.kind,
    language: it.language,
    status: it.status,
    duration_ms: Number(fv(it, 'duration_ms') ?? 0),
    message: it.message || '',
    artifacts: (Array.isArray(fv(it, 'artifact_refs')) ? fv(it, 'artifact_refs') : []).map((a) => fv(a, 'name')).join(' '),
  }));
  downloadTextFile(`run-${fv(ar.run, 'run_no')}.csv`, toCsv(rows));
}

function formatMillis(ms) {
  const n = Number(ms) || 0;
  if (n <= 0) return '—';
  if (n < 1000) return `${Math.round(n)} ms`;
  const secs = n / 1000;
  if (secs < 60) return `${secs.toFixed(2)} s`;
  return `${Math.floor(secs / 60)}m ${Math.round(secs % 60)}s`;
}

function formatNum(value) {
  const n = Number(value) || 0;
  return n >= 100 ? String(Math.round(n)) : String(Math.round(n * 10) / 10);
}

// mm:ss label for the perf timeline axis (offset from the run start).
function formatElapsed(secs) {
  const n = Math.max(0, Math.round(Number(secs) || 0));
  return `${Math.floor(n / 60)}:${String(n % 60).padStart(2, '0')}`;
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

// The tester's queue inside a run: items assigned to them plus unclaimed pool
// items, in execution order. Drives the "case i of n" counter and prev/next.
function buildExecQueue(ex, item) {
  const items = Array.isArray(ex.runItems) ? ex.runItems : [];
  const mine = items
    .filter((it) => {
      if (fv(it, 'item_id') === fv(item, 'item_id')) return true;
      const assignee = fv(it, 'assigned_to');
      return isMe(assignee) || assignee === '';
    })
    .sort((a, b) => Number(fv(a, 'position') ?? 0) - Number(fv(b, 'position') ?? 0));
  const index = Math.max(0, mine.findIndex((it) => fv(it, 'item_id') === fv(item, 'item_id')));
  return {
    total: mine.length,
    index,
    prev: mine[index - 1] ? fv(mine[index - 1], 'item_id') : null,
    next: mine[index + 1] ? fv(mine[index + 1], 'item_id') : null,
  };
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
    // Run context (mockup T09): which run this belongs to and where the case
    // sits in the tester's queue.
    const runId = fv(ex.item, 'run_id');
    if (runId) {
      const runResp = await ApiBinary.one('projectStudioRunGetRequest', { projectId: projectId(), runId });
      ex.run = runResp.run || null;
      ex.runItems = Array.isArray(runResp.items) ? runResp.items : [];
    }
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
  const execQueue = buildExecQueue(ex, item);

  host.innerHTML = `
    <div class="ps-editor-head">
      <tf-button variant="ghost" icon="chevron-left" id="ps-exec-back">${escapeHtml(t('exec_back'))}</tf-button>
      <div class="ps-editor-title-static">
        <div class="ps-detail-name">${escapeHtml(fv(item, 'case_title'))}</div>
        <div class="ps-detail-sub">
          ${escapeHtml(t('exec_sub', { version: Number(fv(item, 'case_version') ?? 1) }))}
          ${ex.run ? ` · ${escapeHtml(t('exec_run_context', {
            no: fv(ex.run, 'run_no'),
            suite: fv(ex.run, 'suite_name') || t('runs_adhoc'),
            by: fv(ex.run, 'created_by_name') || '',
          }))}` : ''}
          ${execQueue.total > 1 ? ` · ${escapeHtml(t('exec_position', { index: execQueue.index + 1, total: execQueue.total }))}` : ''}
        </div>
      </div>
      <div class="ps-editor-badges">
        <tf-chip status="${ITEM_STATUS_CHIP[item.status] || 'info'}" dot>${escapeHtml(t(`item_status_${item.status}`))}</tf-chip>
        <tf-chip status="info">${sprite('clock')} <span id="ps-exec-clock">0:00</span></tf-chip>
      </div>
      <div class="ps-editor-actions">
        ${execQueue.total > 1 ? `
          <tf-button variant="ghost" icon="chevron-left" id="ps-exec-prev" ${execQueue.prev ? '' : 'disabled'}>${escapeHtml(t('exec_prev'))}</tf-button>
          <tf-button variant="ghost" icon="chevron-right" id="ps-exec-next" ${execQueue.next ? '' : 'disabled'}>${escapeHtml(t('exec_next'))}</tf-button>
        ` : ''}
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
  byId('ps-exec-prev')?.addEventListener('click', () => { if (execQueue.prev) openExecItem(execQueue.prev); });
  byId('ps-exec-next')?.addEventListener('click', () => { if (execQueue.next) openExecItem(execQueue.next); });
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
      if (att) openAttachmentPreview(att, attachmentOwner('run_step', ex.itemId, ex.steps[i].index));
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
    acknowledgeAttachmentUploads(projectId(), currentUserId(), step.attachments);
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
    btn.addEventListener('click', () => openAttachmentPreview(ex.attachments[Number(btn.dataset.attPreview)], attachmentOwner('run_item', ex.itemId)));
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
  acknowledgeAttachmentUploads(projectId(), currentUserId(), ex.attachments);
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

const REPORT_KINDS = [
  'runs_over_time', 'suite_pass_rate', 'tester_stats', 'source_coverage', 'defects',
  'perf_trend', 'tester_activity',
];

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
    <div id="ps-reports-kpis" class="ps-kpi-grid ps-reports-kpis"></div>
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
    // The performance card carries its own suite picker; every other report
    // follows the toolbar filter.
    suiteId: kind === 'perf_trend' ? (rep.perfSuiteId || rep.suiteId) : rep.suiteId,
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

function reportSectionShell(kind, icon, chartId, extraActions = '') {
  return `
    <tf-section-card class="ps-report-card" title="${escapeAttr(t(`report_${kind}_title`))}" icon="${icon}">
      <span slot="actions">
        ${extraActions}
        <tf-button variant="ghost" size="sm" icon="download" data-report-csv="${kind}" title="${escapeAttr(t('reports_export_csv'))}"></tf-button>
      </span>
      <div id="${chartId}" class="ps-report-body"></div>
    </tf-section-card>
  `;
}

// The four headline numbers of the reports dashboard, derived from the report
// rows already fetched — no extra round trip.
function renderReportKpis() {
  const host = byId('ps-reports-kpis');
  const rep = f2().reports;
  if (!host) return;
  if (!rep.loaded) { host.innerHTML = ''; return; }

  const overTime = rep.data.runs_over_time?.rows || [];
  const totals = overTime.reduce((acc, r) => {
    acc.runs += Number(r.runs ?? 0);
    acc.passed += Number(r.passed ?? 0);
    acc.executed += Number(r.passed ?? 0) + Number(r.failed ?? 0) + Number(r.blocked ?? 0) + Number(r.skipped ?? 0);
    return acc;
  }, { runs: 0, passed: 0, executed: 0 });
  const passRate = totals.executed ? Math.round((totals.passed / totals.executed) * 1000) / 10 : 0;

  const testers = rep.data.tester_stats?.rows || [];
  const avgSeconds = testers.length
    ? testers.reduce((sum, r) => sum + Number(r.avg_duration_secs ?? 0), 0) / testers.length
    : 0;

  // The defects report is grouped by (severity, status); "open" is everything
  // that is not done.
  const defects = (rep.data.defects?.rows || []).filter((r) => String(r.status || '') !== 'done');
  const openDefects = defects.reduce((sum, r) => sum + Number(r.count ?? 0), 0);
  const criticalDefects = defects
    .filter((r) => String(r.severity || '').toLowerCase() === 'critical')
    .reduce((sum, r) => sum + Number(r.count ?? 0), 0);

  host.innerHTML = `
    <tf-stat-card icon="check" label="${escapeAttr(t('report_kpi_pass_rate'))}" value="${passRate}" suffix="%"
      accent="${passRate >= 90 ? 'success' : passRate >= 70 ? 'warning' : 'danger'}"
      delta="${escapeAttr(t('report_kpi_pass_rate_delta', { executed: totals.executed }))}" delta-type="neutral"></tf-stat-card>
    <tf-stat-card icon="play" label="${escapeAttr(t('report_kpi_runs'))}" value="${totals.runs}" accent="info"
      delta="${escapeAttr(t('report_kpi_runs_delta', { days: overTime.length }))}" delta-type="neutral"></tf-stat-card>
    <tf-stat-card icon="clock" label="${escapeAttr(t('report_kpi_avg_time'))}" value="${formatDuration(Math.round(avgSeconds))}"
      delta="${escapeAttr(t('report_kpi_avg_time_delta'))}" delta-type="neutral"></tf-stat-card>
    <tf-stat-card icon="alert" label="${escapeAttr(t('report_kpi_defects'))}" value="${openDefects}"
      accent="${openDefects ? 'danger' : 'success'}"
      delta="${escapeAttr(t('report_kpi_defects_delta', { count: criticalDefects }))}" delta-type="${criticalDefects ? 'warn' : 'neutral'}"></tf-stat-card>
  `;
}

function renderReportSections() {
  renderReportKpis();
  const grid = byId('ps-reports-grid');
  if (!grid) return;
  grid.innerHTML = `
    ${reportSectionShell('runs_over_time', 'chart-line', 'ps-rep-trend')}
    ${reportSectionShell('suite_pass_rate', 'bar-chart', 'ps-rep-results')}
    ${reportSectionShell('tester_stats', 'users', 'ps-rep-testers')}
    ${reportSectionShell('source_coverage', 'database', 'ps-rep-coverage')}
    ${reportSectionShell('defects', 'alert', 'ps-rep-defects')}
    ${reportSectionShell('perf_trend', 'chart-line', 'ps-rep-perf', `
      <tf-button variant="ghost" size="sm" icon="grid-rows" data-perf-compare>${escapeHtml(t('report_perf_compare'))}</tf-button>
    `)}
    ${reportSectionShell('tester_activity', 'users', 'ps-rep-activity')}
  `;
  // renderReportSections may run twice on the same grid element (initial view
  // + after "Generate") — wire the CSV delegate only once.
  if (!grid.dataset.csvWired) {
    grid.dataset.csvWired = '1';
    grid.addEventListener('click', (e) => {
      if (e.target.closest('[data-perf-compare]')) {
        openPerfCompareWindow();
        return;
      }
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

  // 1) Trend — pass-rate line over time.
  const trendHost = byId('ps-rep-trend');
  const trendRows = reportRows('runs_over_time', trendHost);
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
  const resultRows = reportRows('suite_pass_rate', resultsHost);
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
  const testerRows = reportRows('tester_stats', testersHost);
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
  const coverageRows = reportRows('source_coverage', coverageHost);
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
  const defectRows = reportRows('defects', defectsHost);
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

  // 6) Performance trend (F4) and 7) tester activity (F4).
  renderPerfReport();
  renderTesterActivityReport();
}

// Writes the "no data" / error placeholder into `hostEl` and returns null, or
// hands back the rows when the report has content.
function reportRows(kind, hostEl) {
  const data = f2().reports.data[kind] || { rows: [], error: null };
  if (data.error) {
    hostEl.innerHTML = `<div class="ps-form-error">${escapeHtml(data.error)}</div>`;
    return null;
  }
  if (!data.rows.length) {
    hostEl.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('reports_no_data'))}</div>`;
    return null;
  }
  return data.rows;
}

// =============================================================================
// T14 (F4) — report 7: performance trend + run comparison
// =============================================================================

// perf_trend rows are one per (run, endpoint): {run_id, run_no, started_at,
// endpoint, p50_ms, p90_ms, p99_ms, rps, failures, requests, users}.
function perfRuns(rows) {
  const byRun = new Map();
  for (const row of rows) {
    const runId = String(rowVal(row, ['run_id'], ''));
    if (!runId || byRun.has(runId)) continue;
    byRun.set(runId, {
      runId,
      runNo: Number(rowVal(row, ['run_no'], 0)),
      startedAt: String(rowVal(row, ['started_at'], '')),
    });
  }
  return [...byRun.values()].sort((a, b) => String(a.startedAt).localeCompare(String(b.startedAt)));
}

function perfEndpoints(rows) {
  return [...new Set(rows.map((row) => String(rowVal(row, ['endpoint'], ''))).filter(Boolean))];
}

function renderPerfReport() {
  const s = f2();
  const rep = s.reports;
  const host = byId('ps-rep-perf');
  if (!host) return;
  const rows = reportRows('perf_trend', host);
  if (!rows) return;

  const runs = perfRuns(rows);
  const endpoints = perfEndpoints(rows);
  if (!endpoints.includes(rep.perfEndpoint)) rep.perfEndpoint = endpoints[0] || '';
  const latest = runs[runs.length - 1] || null;
  const latestRows = latest ? rows.filter((row) => String(rowVal(row, ['run_id'], '')) === latest.runId) : [];

  host.innerHTML = `
    <div class="ps-perf-report-bar">
      <tf-select id="ps-rep-perf-suite" label="${escapeAttr(t('reports_suite'))}" value="${escapeAttr(rep.perfSuiteId)}">
        <option value="" ${rep.perfSuiteId ? '' : 'selected'}>${escapeHtml(t('reports_suite_all'))}</option>
        ${s.suites.map((su) => {
          const sid = fv(su, 'suite_id');
          return `<option value="${escapeAttr(sid)}" ${sid === rep.perfSuiteId ? 'selected' : ''}>${escapeHtml(su.name)}</option>`;
        }).join('')}
      </tf-select>
      <tf-select id="ps-rep-perf-endpoint" label="${escapeAttr(t('report_perf_endpoint'))}" value="${escapeAttr(rep.perfEndpoint)}">
        ${endpoints.map((ep) => `<option value="${escapeAttr(ep)}" ${ep === rep.perfEndpoint ? 'selected' : ''}>${escapeHtml(ep)}</option>`).join('')}
      </tf-select>
    </div>
    <div id="ps-rep-perf-chart"></div>
    <div class="ps-report-subtitle">${escapeHtml(latest
      ? t('report_perf_latest', { no: latest.runNo, at: formatTimestamp(latest.startedAt) })
      : t('reports_no_data'))}</div>
    <div id="ps-rep-perf-table"></div>
  `;

  byId('ps-rep-perf-suite')?.addEventListener('change', async (e) => {
    rep.perfSuiteId = e.detail?.value ?? e.target.value ?? '';
    await reloadPerfTrend();
  });
  byId('ps-rep-perf-endpoint')?.addEventListener('change', (e) => {
    rep.perfEndpoint = e.detail?.value ?? e.target.value ?? '';
    renderPerfReport();
  });

  const series = [
    { key: 'p50_ms', tone: 'success' },
    { key: 'p90_ms', tone: 'warning' },
    { key: 'p99_ms', tone: 'critical' },
  ];
  const chart = document.createElement('tf-line-chart');
  chart.height = 220;
  chart.xAxis = { scale: 'category', min: null, max: null, ticks: null, format: null };
  chart.yAxis = { scale: 'linear', min: 0, max: null, ticks: 5, format: (v) => `${Math.round(v)}` };
  chart.series = series.map((sr) => ({
    id: sr.key,
    name: t(`report_perf_${sr.key}`),
    tone: sr.tone,
    style: 'solid',
    showInLegend: true,
    points: runs.map((run) => {
      const row = rows.find((r) => String(rowVal(r, ['run_id'], '')) === run.runId
        && String(rowVal(r, ['endpoint'], '')) === rep.perfEndpoint);
      return { x: `#${run.runNo}`, y: Number(rowVal(row || {}, [sr.key], 0)) };
    }),
  }));
  byId('ps-rep-perf-chart')?.replaceChildren(chart);

  const tableHost = byId('ps-rep-perf-table');
  if (!tableHost || !latestRows.length) return;
  tableHost.innerHTML = `
    <tf-table>
      <tf-column key="endpoint" label="${escapeAttr(t('report_perf_endpoint'))}"></tf-column>
      <tf-column key="p50" label="${escapeAttr(t('report_perf_p50_ms'))}" renderer="num"></tf-column>
      <tf-column key="p90" label="${escapeAttr(t('report_perf_p90_ms'))}" renderer="num"></tf-column>
      <tf-column key="p99" label="${escapeAttr(t('report_perf_p99_ms'))}" renderer="num"></tf-column>
      <tf-column key="rps" label="${escapeAttr(t('report_perf_rps'))}" renderer="num"></tf-column>
      <tf-column key="failures" label="${escapeAttr(t('report_perf_failures'))}" renderer="num"></tf-column>
    </tf-table>
  `;
  tableHost.querySelector('tf-table').rows = latestRows.map((row) => ({
    endpoint: String(rowVal(row, ['endpoint'], '—')),
    p50: formatNum(rowVal(row, ['p50_ms'], 0)),
    p90: formatNum(rowVal(row, ['p90_ms'], 0)),
    p99: formatNum(rowVal(row, ['p99_ms'], 0)),
    rps: formatNum(rowVal(row, ['rps'], 0)),
    failures: Number(rowVal(row, ['failures'], 0)),
  }));
}

// Re-queries only perf_trend: the other six reports keep the toolbar filters.
async function reloadPerfTrend() {
  const s = f2();
  const rep = s.reports;
  try {
    const resp = await ApiBinary.one('projectStudioReportQueryRequest', {
      projectId: projectId(),
      report: 'perf_trend',
      fromDate: rep.from,
      toDate: rep.to,
      suiteId: rep.perfSuiteId || rep.suiteId,
    });
    const parsed = JSON.parse(fv(resp, 'rows_json') || '[]');
    rep.data.perf_trend = { rows: Array.isArray(parsed) ? parsed : [], error: null };
  } catch (err) {
    rep.data.perf_trend = { rows: [], error: err.message };
  }
  rep.perfEndpoint = '';
  if (state.tab === 'tests' && s.view === 'reports') renderPerfReport();
}

// Compare window: two perf runs -> per-endpoint deltas. `status` marks an
// endpoint that exists in only one of the two runs.
function openPerfCompareWindow() {
  const rep = f2().reports;
  const runs = perfRuns(rep.data.perf_trend?.rows || []);
  if (runs.length < 2) {
    toast(t('report_perf_compare_need_two'), 'info');
    return;
  }
  const { body, foot, cleanup } = openWindow({
    title: t('report_perf_compare_title'),
    subtitle: t('report_perf_compare_sub'),
    icon: 'grid-2x2',
    width: 820,
  });
  const label = (run) => `#${run.runNo} · ${formatTimestamp(run.startedAt)}`;
  const optionsFor = (selected) => runs
    .map((run) => `<option value="${escapeAttr(run.runId)}" ${run.runId === selected ? 'selected' : ''}>${escapeHtml(label(run))}</option>`)
    .join('');
  const initialA = runs[runs.length - 2].runId;
  const initialB = runs[runs.length - 1].runId;

  body.innerHTML = `
    <div class="ps-perf-compare-bar">
      <tf-select id="ps-perf-a" label="${escapeAttr(t('report_perf_run_a'))}" value="${escapeAttr(initialA)}">${optionsFor(initialA)}</tf-select>
      <tf-select id="ps-perf-b" label="${escapeAttr(t('report_perf_run_b'))}" value="${escapeAttr(initialB)}">${optionsFor(initialB)}</tf-select>
      <tf-button variant="primary" icon="chart-line" data-action="compare">${escapeHtml(t('report_perf_compare_run'))}</tf-button>
    </div>
    <div id="ps-perf-compare-host"><div class="ps-field-hint">${escapeHtml(t('report_perf_compare_hint'))}</div></div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="close-compare">${escapeHtml(t('action_close'))}</tf-button>
    </div>
  `;
  foot.addEventListener('click', (e) => {
    if (e.target.closest('[data-action="close-compare"]')) cleanup();
  });

  const showError = (msg) => {
    const el = body.querySelector('[data-form-error]');
    if (el) { el.hidden = !msg; el.textContent = msg || ''; }
  };

  body.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action="compare"]');
    if (!btn) return;
    const runA = String(body.querySelector('#ps-perf-a')?.value ?? '');
    const runB = String(body.querySelector('#ps-perf-b')?.value ?? '');
    if (!runA || !runB || runA === runB) { showError(t('report_perf_compare_same')); return; }
    showError(null);
    const host = body.querySelector('#ps-perf-compare-host');
    host.innerHTML = `<div class="ps-loading"><tf-spinner size="sm"></tf-spinner> ${escapeHtml(t('reports_loading'))}</div>`;
    let rows = [];
    try {
      const resp = await ApiBinary.one('projectStudioReportQueryRequest', {
        projectId: projectId(),
        report: 'perf_compare',
        runIds: [runA, runB],
      });
      const parsed = JSON.parse(fv(resp, 'rows_json') || '[]');
      rows = Array.isArray(parsed) ? parsed : [];
    } catch (err) {
      host.innerHTML = `<div class="ps-form-error">${escapeHtml(err.message)}</div>`;
      return;
    }
    if (!rows.length) {
      host.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('reports_no_data'))}</div>`;
      return;
    }
    renderPerfCompareTable(host, rows);
  });
}

function renderPerfCompareTable(host, rows) {
  const metric = (side, key) => (side && side[key] != null ? formatNum(side[key]) : '—');
  // A negative delta is faster, so "down" is the good direction here.
  const deltaCell = (delta, key) => {
    const value = delta && delta[key] != null ? Number(delta[key]) : null;
    if (value === null || !Number.isFinite(value)) return '<span>—</span>';
    const tone = value > 1 ? 'ps-delta-worse' : (value < -1 ? 'ps-delta-better' : 'ps-delta-flat');
    const sign = value > 0 ? '+' : '';
    return `<span class="${tone}">${escapeHtml(`${sign}${formatNum(value)}%`)}</span>`;
  };
  host.innerHTML = `
    <table class="ps-compare-table">
      <thead>
        <tr>
          <th>${escapeHtml(t('report_perf_endpoint'))}</th>
          <th>${escapeHtml(t('report_perf_p50_ms'))}</th>
          <th>${escapeHtml(t('report_perf_p90_ms'))}</th>
          <th>${escapeHtml(t('report_perf_p99_ms'))}</th>
          <th>${escapeHtml(t('report_perf_rps'))}</th>
        </tr>
      </thead>
      <tbody>
        ${rows.map((row) => {
          const status = String(rowVal(row, ['status'], '') || '');
          const a = row.a || {};
          const b = row.b || {};
          const delta = row.delta_pct || row.deltaPct || {};
          const badge = status
            ? `<tf-chip status="${status === 'added' ? 'ok' : 'warn'}">${escapeHtml(t(`report_perf_status_${status}`))}</tf-chip>`
            : '';
          const cell = (key) => `
            <td>
              <div class="ps-compare-values"><b>${escapeHtml(metric(a, key))}</b> → <b>${escapeHtml(metric(b, key))}</b></div>
              <div class="ps-compare-delta">${deltaCell(delta, key)}</div>
            </td>
          `;
          return `
            <tr class="${status ? `is-${escapeAttr(status)}` : ''}">
              <td><div class="ps-compare-endpoint">${escapeHtml(String(rowVal(row, ['endpoint'], '—')))}</div>${badge}</td>
              ${cell('p50_ms')}${cell('p90_ms')}${cell('p99_ms')}${cell('rps')}
            </tr>
          `;
        }).join('')}
      </tbody>
    </table>
  `;
}

// =============================================================================
// T14 (F4) — report 8: tester activity
// =============================================================================

function renderTesterActivityReport() {
  const host = byId('ps-rep-activity');
  if (!host) return;
  const rows = reportRows('tester_activity', host);
  if (!rows) return;

  // Rows are per (tester, day); the table shows the tester totals and the bar
  // chart the executions per day across the whole team.
  const byTester = new Map();
  const byDay = new Map();
  for (const row of rows) {
    const userId = String(rowVal(row, ['user_id'], '')) || String(rowVal(row, ['display_name'], ''));
    const executed = Number(rowVal(row, ['executed'], 0));
    const entry = byTester.get(userId) || {
      name: String(rowVal(row, ['display_name'], '') || '—'),
      executed: 0, passed: 0, failed: 0, blocked: 0, skipped: 0, approvals: 0, durationSum: 0, durationDays: 0,
    };
    entry.executed += executed;
    entry.passed += Number(rowVal(row, ['passed'], 0));
    entry.failed += Number(rowVal(row, ['failed'], 0));
    entry.blocked += Number(rowVal(row, ['blocked'], 0));
    entry.skipped += Number(rowVal(row, ['skipped'], 0));
    entry.approvals += Number(rowVal(row, ['approvals'], 0));
    const avg = Number(rowVal(row, ['avg_duration_secs'], 0));
    if (avg > 0 && executed > 0) { entry.durationSum += avg * executed; entry.durationDays += executed; }
    byTester.set(userId, entry);

    const day = String(rowVal(row, ['day'], ''));
    if (day) byDay.set(day, (byDay.get(day) || 0) + executed);
  }

  host.innerHTML = '<div id="ps-rep-activity-chart"></div><div id="ps-rep-activity-table"></div>';

  const days = [...byDay.keys()].sort();
  if (days.length) {
    const chart = document.createElement('tf-bar-chart');
    chart.height = 200;
    chart.xAxis = { scale: 'category', min: null, max: null, ticks: null, format: null };
    chart.yAxis = { scale: 'linear', min: 0, max: null, ticks: 4, format: null };
    chart.series = [{
      id: 'executed',
      name: t('report_activity_executed'),
      tone: 'accent',
      style: 'solid',
      showInLegend: true,
      points: days.map((day) => ({ x: day, y: byDay.get(day) })),
    }];
    byId('ps-rep-activity-chart')?.replaceChildren(chart);
  }

  const tableHost = byId('ps-rep-activity-table');
  if (!tableHost) return;
  tableHost.innerHTML = `
    <tf-table>
      <tf-column key="tester" label="${escapeAttr(t('report_col_tester'))}"></tf-column>
      <tf-column key="executed" label="${escapeAttr(t('report_activity_executed'))}" renderer="num"></tf-column>
      <tf-column key="avg" label="${escapeAttr(t('report_col_avg_time'))}"></tf-column>
      <tf-column key="approvals" label="${escapeAttr(t('report_activity_approvals'))}" renderer="num"></tf-column>
      <tf-column key="passRate" label="${escapeAttr(t('report_activity_pass_rate'))}"></tf-column>
    </tf-table>
  `;
  tableHost.querySelector('tf-table').rows = [...byTester.values()]
    .sort((a, b) => b.executed - a.executed)
    .map((entry) => {
      const graded = entry.passed + entry.failed + entry.blocked + entry.skipped;
      return {
        tester: entry.name,
        executed: entry.executed,
        avg: formatDuration(entry.durationDays > 0 ? Math.round(entry.durationSum / entry.durationDays) : 0),
        approvals: entry.approvals,
        passRate: graded > 0 ? `${Math.round((entry.passed / graded) * 1000) / 10}%` : '—',
      };
    });
}

// =============================================================================
// Z01 — tasks & defects list
// =============================================================================

function taskNoLabel(task) {
  return fv(task, 'task_key');
}

async function loadTaskTypes(isCurrent = () => true) {
  const id = projectId();
  const response = await ApiBinary.one('projectStudioTaskTypesListRequest', { projectId: id });
  if (projectId() !== id || !isCurrent()) return;
  state.taskTypes = response.types;
  state.taskTypeSourceProjectId = response.source_project_id;
  return state.taskTypes;
}

function taskTypeName(typeId) {
  const type = state.taskTypes.find((entry) => fv(entry, 'type_id') === typeId);
  return type ? taskTypeLabel(type, t) : t('task_history_unavailable_type');
}

async function renderTaskTypes(host) {
  if (!host) return;
  try { await loadTaskTypes(); }
  catch (error) { host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('task_types_failed')}: ${error.message}`)}</div>`; return; }
  const mutable = canArea('tasks', 'admin') && !state.project.inherit_task_types;
  host.classList.add('ps-task-types');
  host.innerHTML = `${state.project.inherit_task_types ? `<div class="ps-banner-info">${escapeHtml(t('inheritance_types_readonly', { project: [...state.projects, ...state.breadcrumbs].find((row) => row.project_id === state.taskTypeSourceProjectId)?.name || t('subproject_parent') }))}</div>` : ''}<div class="ps-members-toolbar"><span class="ps-field-hint">${escapeHtml(t('task_types_hint'))}</span>${mutable ? `<tf-button variant="primary" icon="plus" data-new-type>${escapeHtml(t('task_type_new'))}</tf-button>` : ''}</div><tf-table><tf-column key="name" label="${escapeAttr(t('task_type_name'))}" renderer="html"></tf-column><tf-column key="description" label="${escapeAttr(t('task_type_description'))}"></tf-column><tf-column key="active" label="${escapeAttr(t('task_type_state'))}" renderer="chip"></tf-column><tf-column key="order" label="${escapeAttr(t('task_type_order'))}" renderer="num"></tf-column></tf-table>`;
  const table = host.querySelector('tf-table');
  table.rows = state.taskTypes.map((type) => ({ _id: type.type_id, name: `<div class="ps-task-type-name">${escapeHtml(taskTypeLabel(type, t))}</div><div class="ps-field-hint">${escapeHtml(t(type.built_in ? 'task_type_builtin' : 'task_type_custom'))} · ${escapeHtml(type.type_id)}</div>`, description: taskTypeDescription(type, t), active: chipCell(type.active ? 'ok' : 'info', t(type.active ? 'task_type_active' : 'task_type_inactive')), order: type.sort_order }));
  table.rowActions = (row) => {
    const type = state.taskTypes.find((entry) => entry.type_id === row._id);
    if (!mutable || type.built_in) return null;
    const button = document.createElement('tf-button');
    button.setAttribute('variant', 'ghost'); button.setAttribute('size', 'sm'); button.setAttribute('icon', 'more'); button.setAttribute('title', t('action_more'));
    button.addEventListener('click', (event) => {
      event.stopPropagation();
      openActionMenu(button, [
        { id: 'edit', label: t('action_edit'), icon: 'edit', run: () => openTaskTypeEditor(type, () => renderTaskTypes(host)) },
        { id: 'active', label: t(type.active ? 'task_type_deactivate' : 'task_type_activate'), icon: type.active ? 'pause' : 'play', run: async () => {
          try { await ApiBinary.one('projectStudioTaskTypeSaveRequest', { projectId: projectId(), typeId: type.type_id, name: type.name, description: type.description, sortOrder: type.sort_order, active: !type.active }); await renderTaskTypes(host); }
          catch (error) { toast(`${t('task_types_failed')}: ${error.message}`, 'error'); }
        } },
      ]);
    });
    return button;
  };
  host.querySelector('[data-new-type]')?.addEventListener('click', () => openTaskTypeEditor(null, () => renderTaskTypes(host)));
}

function openTaskTypesWindow() {
  if (!canArea('tasks', 'admin')) return;
  const { body, foot, cleanup } = openWindow({ title: t('task_types_title'), icon: 'list', width: 980 });
  foot.innerHTML = `<div></div><tf-button variant="ghost" data-action="close">${escapeHtml(t('action_close'))}</tf-button>`;
  foot.querySelector('[data-action]').addEventListener('click', cleanup);
  renderTaskTypes(body);
}

function openTaskTypeEditor(type, onSaved) {
  if (!canArea('tasks', 'admin') || state.project.inherit_task_types || type?.built_in) return;
  const { body, foot, cleanup } = openWindow({ title: t(type ? 'task_type_edit' : 'task_type_new'), icon: 'list', width: 600 });
  body.innerHTML = `<tf-input id="ps-type-id" label="${escapeAttr(t('task_type_id'))}" hint="${escapeAttr(t('task_type_id_hint'))}" value="${escapeAttr(type?.type_id || '')}" ${type ? 'readonly' : ''}></tf-input><tf-input id="ps-type-name" label="${escapeAttr(t('task_type_name'))}" value="${escapeAttr(type?.name || '')}"></tf-input><tf-textarea id="ps-type-desc" label="${escapeAttr(t('task_type_description'))}" rows="3"></tf-textarea><tf-input id="ps-type-order" type="number" min="0" label="${escapeAttr(t('task_type_order'))}" value="${type?.sort_order ?? (Math.max(0, ...state.taskTypes.map((entry) => entry.sort_order)) + 10)}"></tf-input><div class="ps-toggle-inline"><tf-toggle id="ps-type-active" ${!type || type.active ? 'checked' : ''}></tf-toggle><span>${escapeHtml(t('task_type_active'))}</span></div><div class="ps-form-error" data-form-error hidden></div>`;
  body.querySelector('#ps-type-desc').value = type?.description || '';
  foot.innerHTML = `<div></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" data-action="save">${escapeHtml(t('action_save'))}</tf-button></div>`;
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]');
    if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    const typeId = body.querySelector('#ps-type-id').value.trim();
    const name = body.querySelector('#ps-type-name').value.trim();
    const errorEl = body.querySelector('[data-form-error]');
    if (!/^[a-z][a-z0-9_]{1,31}$/.test(typeId) || !name || name.length > 80 || body.querySelector('#ps-type-desc').value.length > 500) { errorEl.hidden = false; errorEl.textContent = t('err_task_type'); return; }
    button.setAttribute('disabled', '');
    try { await ApiBinary.one('projectStudioTaskTypeSaveRequest', { projectId: projectId(), typeId, name, description: body.querySelector('#ps-type-desc').value.trim(), sortOrder: Number(body.querySelector('#ps-type-order').value), active: body.querySelector('#ps-type-active').hasAttribute('checked') }); await onSaved(); cleanup(); }
    catch (error) { button.removeAttribute('disabled'); errorEl.hidden = false; errorEl.textContent = `${t('task_types_failed')}: ${error.message}`; }
  });
}

function taskSourceProject(task) {
  return state.projects.find((project) => project.project_id === task.project_id);
}

function taskRowTypeName(task) {
  const builtIn = ['feature', 'defect', 'technical', 'security', 'subtask', 'epic'].includes(task.task_type);
  return taskTypeLabel({ type_id: task.task_type, built_in: builtIn, name: task.task_type_name }, t);
}

async function openTaskRecord(task, fields = {}) {
  await openProject(task.project_id, { tab: 'tasks', taskId: task.task_id, ...fields });
}

async function loadTaskScopeProjects(isCurrent) {
  const id = projectId();
  const tv = state.tasksView;
  const response = await ApiBinary.one('projectStudioProjectTreeRequest', { projectId: id, includeEnded: false, includeArchived: true });
  if (projectId() !== id || tv !== state.tasksView || !isCurrent()) return;
  const previous = new Set(descendantsOf(id).map((row) => row.project_id));
  state.projects = [...state.projects.filter((row) => !previous.has(row.project_id)), ...response.projects];
  state.breadcrumbs = [...new Map([...state.breadcrumbs, ...response.breadcrumbs].map((row) => [row.project_id, row])).values()];
  const current = response.projects.find((row) => row.project_id === id);
  if (current) state.project = current;
  const allowed = new Set(response.projects.map((row) => row.project_id));
  tv.sourceProjectIds = tv.sourceProjectIds.filter((projectId) => allowed.has(projectId));
}

function taskListFields(tv, id) {
  return { projectId: id, scope: tv.scope, sourceProjectIds: tv.sourceProjectIds, taskType: tv.filters.type,
    assignedTo: tv.filters.mine ? 'me' : '', search: tv.filters.search, severity: tv.filters.severity, includeArchived: tv.filters.includeArchived };
}

async function loadTasksPage(isCurrent = () => true) {
  const tv = state.tasksView || (state.tasksView = freshTasksState());
  const id = projectId();
  const generation = tv.renderGeneration;
  const response = await ApiBinary.one('projectStudioTasksListRequest', { ...taskListFields(tv, id), status: tv.filters.status, offset: (tv.page - 1) * F2_PAGE_SIZE, limit: F2_PAGE_SIZE });
  if (projectId() !== id || tv !== state.tasksView || generation !== tv.renderGeneration || !isCurrent()) return;
  tv.rows = response.tasks; tv.total = response.total; tv.summary = response.summary;
}

async function loadTasksBoard(append = false, isCurrent = () => true) {
  const tv = state.tasksView || (state.tasksView = freshTasksState());
  const id = projectId();
  const generation = tv.renderGeneration;
  const response = await ApiBinary.one('projectStudioTasksListRequest', { ...taskListFields(tv, id), status: '', offset: append ? tv.boardRows.length : 0, limit: BOARD_PAGE_SIZE });
  if (projectId() !== id || tv !== state.tasksView || generation !== tv.renderGeneration || !isCurrent()) return;
  tv.boardRows = append ? [...tv.boardRows, ...response.tasks] : response.tasks;
  tv.total = response.total; tv.summary = response.summary;
}

async function renderTasksTab() {
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  const tv = state.tasksView || (state.tasksView = freshTasksState());
  const id = projectId();
  const generation = ++tv.renderGeneration;
  const isCurrentPanel = () => state.tab === 'tasks' && projectId() === id && state.tasksView === tv &&
    panel.isConnected && byId('ps-tab-panel') === panel;
  const isCurrent = () => isCurrentPanel() && tv.renderGeneration === generation;
  if (!canArea(tv.mode === 'board' ? 'board' : 'tasks')) tv.mode = canArea('tasks') ? 'list' : 'board';
  const board = tv.mode === 'board';
  await ensureF2Members();
  if (!isCurrent()) return;
  try {
    await loadTaskScopeProjects(isCurrent);
    if (!isCurrent()) return;
    await loadTaskTypes(isCurrent);
    if (!isCurrent()) return;
    if (board) await loadTasksBoard(false, isCurrent);
    else await loadTasksPage(isCurrent);
  } catch (err) {
    if (!isCurrent()) return;
    panel.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('tasks_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (!isCurrent()) return;

  panel.innerHTML = `
    <div class="ps-tests-toolbar">
      <tf-segmented id="ps-tasks-mode" value="${escapeAttr(tv.mode)}">
        <option value="list" ${canArea('tasks') ? '' : 'disabled'}>${escapeHtml(t('tasks_view_list'))}</option>
        <option value="board" ${canArea('board') ? '' : 'disabled'}>${escapeHtml(t('tasks_view_board'))}</option>
      </tf-segmented>
      <tf-segmented id="ps-tasks-scope" value="${tv.scope}"><option value="single">${escapeHtml(t('scope_single'))}</option><option value="descendants">${escapeHtml(t('scope_descendants'))}</option></tf-segmented>
      ${tv.scope === 'descendants' ? `<tf-multiselect id="ps-tasks-project-filter" label="${escapeAttr(t('scope_project_filter'))}" placeholder="${escapeAttr(t('scope_all_projects'))}"></tf-multiselect>` : ''}
      <tf-searchbox id="ps-tasks-search" placeholder="${escapeAttr(t('tasks_search_placeholder'))}" debounce="300" value="${escapeAttr(tv.filters.search)}"></tf-searchbox>
      <tf-select id="ps-tasks-f-type" label="${escapeAttr(t('tasks_col_type'))}" value="${escapeAttr(tv.filters.type)}">
        <option value="">${escapeHtml(t('tasks_filter_all'))}</option>
        ${state.taskTypes.map((type) => `<option value="${escapeAttr(type.type_id)}" ${type.type_id === tv.filters.type ? 'selected' : ''}>${escapeHtml(taskTypeLabel(type, t))}</option>`).join('')}
      </tf-select>
      ${board ? '' : `
        <tf-select id="ps-tasks-f-status" value="${escapeAttr(tv.filters.status)}">
          <option value="" ${tv.filters.status === '' ? 'selected' : ''}>${escapeHtml(t('tasks_filter_status_all'))}</option>
          ${TASK_STATUSES.map((x) => `<option value="${x}" ${x === tv.filters.status ? 'selected' : ''}>${escapeHtml(t(`task_status_${x}`))}</option>`).join('')}
        </tf-select>
      `}
      <tf-select id="ps-tasks-f-severity" value="${escapeAttr(tv.filters.severity || '')}">
        <option value="">${escapeHtml(t('tasks_filter_severity_all'))}</option>
        ${PRIORITIES.slice().reverse().map((x) => `<option value="${x}" ${x === tv.filters.severity ? 'selected' : ''}>${escapeHtml(t(`sev_${x}`))}</option>`).join('')}
      </tf-select>
      <div class="ps-toggle-inline">
        <tf-toggle id="ps-tasks-f-mine" ${tv.filters.mine ? 'checked' : ''}></tf-toggle>
        <span>${escapeHtml(t('tasks_filter_mine'))}</span>
      </div>
      <div class="ps-toggle-inline">
        <tf-toggle id="ps-tasks-f-archived" ${tv.filters.includeArchived ? 'checked' : ''}></tf-toggle>
        <span>${escapeHtml(t('tasks_filter_archived'))}</span>
      </div>
      <span class="ps-toolbar-spacer"></span>
      <tf-button variant="ghost" icon="database" id="ps-tasks-storage">${escapeHtml(t('attachment_usage_title'))}</tf-button>
      ${canArea('tasks', 'admin') ? `<tf-button variant="ghost" icon="settings" id="ps-tasks-types">${escapeHtml(t('task_types_title'))}</tf-button>` : ''}
      ${canCreateTask(projectAccess()) ? `<tf-button variant="primary" icon="plus" id="ps-tasks-new">${escapeHtml(t('tasks_new'))}</tf-button>` : ''}
    </div>
    <div class="ps-task-index-state" id="ps-task-index-state"></div>
    <div id="ps-tasks-table-host">
      ${(board ? tv.boardRows : tv.rows).length ? '' : `<tf-empty-state icon="check" title="${escapeAttr(t('tasks_empty'))}"></tf-empty-state>`}
    </div>
  `;

  const toolbar = panel.querySelector('.ps-tests-toolbar');
  const isActive = () => isCurrentPanel() && toolbar.isConnected;
  const reload = () => { if (!isActive()) return; tv.page = 1; renderTasksTab(); };
  panel.querySelector('#ps-tasks-mode')?.addEventListener('change', (e) => {
    if (!isActive()) return;
    const mode = e.detail?.value === 'board' ? 'board' : 'list';
    if (mode === tv.mode || !canArea(mode === 'board' ? 'board' : 'tasks')) return;
    tv.mode = mode;
    writeTasksViewMode(projectId(), mode);
    tv.scope = readTaskScope(currentUserId(), projectId(), mode, descendantsOf(projectId(), false).length > 0);
    tv.sourceProjectIds = [];
    reload();
  });
  panel.querySelector('#ps-tasks-scope').addEventListener('change', (event) => { if (!isActive()) return; tv.scope = event.detail.value; writeTaskScope(currentUserId(), projectId(), tv.mode, tv.scope); tv.sourceProjectIds = []; reload(); });
  const projectFilter = panel.querySelector('#ps-tasks-project-filter');
  if (projectFilter) {
    projectFilter.options = descendantsOf(projectId()).filter((project) => allowsArea(project.access, 'tasks') || allowsArea(project.access, 'board')).map((project) => ({ value: project.project_id, label: project.name }));
    projectFilter.value = tv.sourceProjectIds;
    projectFilter.addEventListener('change', (event) => { if (!isActive()) return; tv.sourceProjectIds = event.detail.value; reload(); });
  }
  renderTaskIndexState();
  panel.querySelector('#ps-tasks-search')?.addEventListener('search', (e) => { if (!isActive()) return; tv.filters.search = String(e.detail?.value ?? ''); reload(); });
  panel.querySelector('#ps-tasks-f-type')?.addEventListener('change', (e) => {
    if (!isActive()) return;
    tv.filters.type = e.detail?.value ?? '';
    reload();
  });
  panel.querySelector('#ps-tasks-f-status')?.addEventListener('change', (e) => { if (!isActive()) return; tv.filters.status = e.detail?.value ?? e.target.value ?? ''; reload(); });
  panel.querySelector('#ps-tasks-f-mine')?.addEventListener('change', (e) => { if (!isActive()) return; tv.filters.mine = !!(e.detail?.checked ?? e.target.checked); reload(); });
  panel.querySelector('#ps-tasks-f-archived')?.addEventListener('change', (e) => { if (!isActive()) return; tv.filters.includeArchived = !!(e.detail?.checked ?? e.target.checked); reload(); });
  panel.querySelector('#ps-tasks-f-severity')?.addEventListener('change', (e) => { if (!isActive()) return; tv.filters.severity = e.detail?.value ?? ''; reload(); });
  panel.querySelector('#ps-tasks-new')?.addEventListener('click', () => openTaskWindow({}));
  panel.querySelector('#ps-tasks-types')?.addEventListener('click', () => openTaskTypesWindow());
  panel.querySelector('#ps-tasks-storage')?.addEventListener('click', () => openAttachmentUsageWindow());

  if (board) {
    renderTaskBoard();
    return;
  }
  if (!tv.rows.length) return;
  panel.querySelector('#ps-tasks-table-host').innerHTML = `
    <tf-table id="ps-tasks-table" page-size="${F2_PAGE_SIZE}" total="${tv.total}" page="${tv.page}">
      <tf-column key="no" label="#"></tf-column>
      <tf-column key="title" label="${escapeAttr(t('tasks_col_title'))}"></tf-column>
      ${tv.scope === 'descendants' ? `<tf-column key="project" label="${escapeAttr(t('task_source_project'))}"></tf-column>` : ''}
      <tf-column key="type" label="${escapeAttr(t('tasks_col_type'))}" renderer="chip"></tf-column>
      <tf-column key="severity" label="${escapeAttr(t('tasks_col_severity'))}" renderer="chip"></tf-column>
      <tf-column key="priority" label="${escapeAttr(t('tasks_col_priority'))}" renderer="chip"></tf-column>
      <tf-column key="status" label="${escapeAttr(t('tasks_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="assignee" label="${escapeAttr(t('tasks_col_assignee'))}"></tf-column>
      <tf-column key="due" label="${escapeAttr(t('tasks_col_due'))}"></tf-column>
      <tf-column key="links" label="${escapeAttr(t('tasks_col_links'))}"></tf-column>
      <tf-column key="comments" label="${escapeAttr(t('tasks_col_comments'))}" renderer="num"></tf-column>
    </tf-table>
  `;
  const table = panel.querySelector('#ps-tasks-table');
  const assignRows = () => {
    table.rows = tv.rows.map((task) => {
      const type = fv(task, 'task_type');
      return {
        _id: fv(task, 'task_id'),
        no: taskNoLabel(task),
        title: task.title, project: task.project_name,
        type: chipCell(type === 'defect' ? 'err' : 'info', taskRowTypeName(task)),
        severity: task.severity
          ? chipCell(PRIORITY_CHIP[task.severity], t(`sev_${task.severity}`))
          : chipCell('info', '—'),
        priority: chipCell(PRIORITY_CHIP[task.priority], t(`prio_${task.priority}`)),
        status: task.archived_at ? chipCell('warn', t('task_archived')) : task.resolution === 'not_pursued' ? chipCell('info', t('task_not_pursued')) : chipCell(TASK_STATUS_CHIP[task.status], t(`task_status_${task.status}`)),
        assignee: fv(task, 'assigned_to_name') || '—',
        due: fv(task, 'due_date') || '—',
        links: taskLinkLabels(task),
        comments: Number(fv(task, 'comment_count') ?? 0),
      };
    });
  };
  assignRows();
  table.addEventListener('row-click', (e) => {
    const taskId = e.detail?.row?._id;
    const task = tv.rows.find((row) => row.task_id === taskId);
    if (task) openTaskRecord(task);
  });
  table.addEventListener('page-change', (e) => {
    if (!isActive()) return;
    tv.page = Number(e.detail?.page ?? 1);
    renderTasksTab();
  });
  syncTasksFooter();
}

function syncTasksFooter() {
  const tv = state.tasksView;
  const host = byId('ps-tasks-table-host');
  if (!host || !tv) return;
  let footer = host.querySelector('.ps-table-footer');
  if (!footer) {
    footer = document.createElement('div');
    footer.className = 'ps-table-footer';
    host.appendChild(footer);
  }
  footer.textContent = t('tasks_scope_footer', { shown: tv.mode === 'board' ? tv.boardRows.length : tv.rows.length, total: tv.total, own: tv.summary.own_total, descendants: tv.summary.descendant_total });
}

function renderTaskIndexState() {
  const host = byId('ps-task-index-state');
  const summary = state.tasksView?.summary;
  if (!host || !summary) return;
  const lag = Math.max(0, Number(summary.source_revision) - Number(summary.applied_revision));
  host.textContent = summary.indexed_at ? t('index_freshness', { date: formatTimestamp(summary.indexed_at) }) : t('index_time_unavailable');
  if (lag) {
    const chip = document.createElement('tf-chip'); chip.setAttribute('status', 'warn'); chip.textContent = t('index_lag', { count: lag }); host.append(' ', chip);
  }
}

// =============================================================================
// Z02 — task / defect window (detail + comments)
// =============================================================================

// opts: { taskId } opens an existing task; without taskId the window is a
// creation form ({ taskType?, links?, titlePrefill?, status? } — the tester
// desk prefills defect links, the kanban prefills the column).
function openAttachmentUsageWindow() {
  if (!(canArea('tasks') || canArea('board') || canArea('tests'))) return;
  const pid = projectId();
  const { body, foot, cleanup } = openWindow({ title: t('attachment_usage_title'), icon: 'database', width: 960 });
  let offset = 0;
  const limit = 25;
  foot.innerHTML = `<div></div><tf-button variant="ghost" data-action="close">${escapeHtml(t('action_close'))}</tf-button>`;
  foot.querySelector('[data-action]').addEventListener('click', cleanup);
  const load = async () => {
    try {
      const result = await ApiBinary.one('projectStudioAttachmentUsageRequest', { projectId: pid, offset, limit });
      if (!body.isConnected) return;
      body.innerHTML = `<div class="ps-attachment-usage-summary"><tf-stat-card label="${escapeAttr(t('attachment_usage_total'))}" value="${escapeAttr(formatBytes(Number(result.total_bytes)))}"></tf-stat-card><tf-stat-card label="${escapeAttr(t('attachment_usage_files'))}" value="${Number(result.file_count)}"></tf-stat-card><tf-stat-card label="${escapeAttr(t('attachment_usage_available'))}" value="${escapeAttr(result.available_bytes == null ? t('attachment_usage_unavailable') : formatBytes(Number(result.available_bytes)))}" accent="${result.warning ? 'danger' : 'info'}"></tf-stat-card></div>${result.warning ? `<div class="ps-form-error">${escapeHtml(t('attachment_usage_warning'))}</div>` : ''}${Number(result.missing_files) ? `<div class="ps-form-error">${escapeHtml(t('attachment_usage_missing', { count: result.missing_files }))}</div>` : ''}<span class="ps-field-hint">${escapeHtml(t('attachment_usage_shared'))}</span><tf-section-card title="${escapeAttr(t('attachment_usage_largest'))}" icon="paperclip"><tf-table data-largest><tf-column key="name" label="${escapeAttr(t('attachment_usage_file'))}"></tf-column><tf-column key="task" label="${escapeAttr(t('attachment_usage_owner'))}"></tf-column><tf-column key="size" label="${escapeAttr(t('attachment_usage_size'))}"></tf-column></tf-table></tf-section-card><tf-section-card title="${escapeAttr(t('attachment_usage_tasks'))}" icon="list"><tf-table data-per-task page-size="${limit}" page="${offset / limit + 1}" total="${result.total_tasks}"><tf-column key="key" label="${escapeAttr(t('attachment_usage_task'))}"></tf-column><tf-column key="count" label="${escapeAttr(t('attachment_usage_files'))}" renderer="num"></tf-column><tf-column key="size" label="${escapeAttr(t('attachment_usage_size'))}"></tf-column></tf-table></tf-section-card><div class="ps-field-hint">${escapeHtml(t('attachment_usage_no_retention'))}</div><div class="ps-form-error" data-form-error hidden></div>`;
      const files = body.querySelector('[data-largest]');
      files.rows = result.largest.map((file, index) => ({ _id: index, name: file.filename, task: file.task_key || t(`attachment_owner_${file.owner_kind}`), size: formatBytes(Number(file.size_bytes)) }));
      files.rowActions = (row) => {
        const file = result.largest[row._id];
        const button = document.createElement('tf-button'); button.setAttribute('variant', 'ghost'); button.setAttribute('size', 'sm'); button.setAttribute('icon', 'download'); button.setAttribute('title', t('attachment_download_original'));
        button.addEventListener('click', async (event) => {
          event.stopPropagation(); button.setAttribute('disabled', '');
          try { await downloadProjectAttachment(attachmentOwner(file.owner_kind, file.owner_id, file.step_index, pid), { sha256: file.sha256, name: file.filename }); }
          catch (error) { const el = body.querySelector('[data-form-error]'); el.hidden = false; el.textContent = attachmentError(error); }
          finally { button.removeAttribute('disabled'); }
        });
        return button;
      };
      const tasks = body.querySelector('[data-per-task]');
      tasks.rows = result.per_task.map((task) => ({ _id: task.task_id, key: task.task_key, count: Number(task.file_count), size: formatBytes(Number(task.total_bytes)) }));
      tasks.addEventListener('page-change', (event) => { offset = (Number(event.detail.page) - 1) * limit; load(); });
      tasks.addEventListener('row-click', (event) => { if (event.detail.row?._id && (canArea('tasks') || canArea('board'))) { cleanup(); openTaskWindow({ taskId: event.detail.row._id }); } });
    } catch (error) { body.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('attachment_usage_failed')}: ${error.message}`)}</div>`; }
  };
  load();
}

function wireTaskUploads({ body, foot, signal, attachments, onAdded, pid }) {
  const host = body.querySelector('#ps-task-uploads');
  if (!host) return { get busy() { return false; }, close() {} };
  const userId = currentUserId();
  const active = new Map();
  let closed = false;
  const showError = (error) => { const el = body.querySelector('[data-form-error]'); if (!el || closed) return; el.hidden = false; el.textContent = `${t('attachment_upload_failed')}: ${attachmentError(error)}`; };
  const syncSave = () => { const button = foot.querySelector('[data-action="save"]'); if (active.size) button?.setAttribute('disabled', ''); else button?.removeAttribute('disabled'); };
  const renderPending = () => {
    if (closed) return;
    host.querySelectorAll('[data-pending-upload]').forEach((row) => row.remove());
    const selected = new Set(attachments.map((attachment) => attachment.sha256));
    for (const record of pendingAttachmentUploads(pid, userId)) {
      if ([...active.values()].some((operation) => operation.uploadId === record.uploadId) || record.complete && selected.has(record.sha256)) continue;
      const row = document.createElement('div'); row.className = 'ps-upload-row'; row.dataset.pendingUpload = record.uploadId;
      row.innerHTML = `<b>${escapeHtml(record.filename)}</b><span class="ps-field-hint">${escapeHtml(t(record.complete ? 'attachment_upload_recoverable' : 'attachment_upload_resume_hint'))}</span><tf-progress-bar value="${record.totalSize ? Math.floor(record.nextOffset / record.totalSize * 100) : record.complete ? 100 : 0}" label="${escapeAttr(`${formatBytes(record.nextOffset)} / ${formatBytes(record.totalSize)}`)}"></tf-progress-bar><div class="ps-task-comment-tools">${record.complete ? `<tf-button variant="primary" data-upload-recover>${escapeHtml(t('attachment_upload_recover'))}</tf-button>` : `<tf-file-input data-upload-resume label="${escapeAttr(t('attachment_upload_resume'))}"></tf-file-input>`}<tf-button variant="ghost" icon="refresh" data-upload-status>${escapeHtml(t('attachment_upload_status'))}</tf-button><tf-button variant="ghost" icon="trash" data-upload-cancel>${escapeHtml(t('attachment_upload_cancel'))}</tf-button></div><span class="ps-form-error" data-upload-error hidden></span>`;
      const fail = (error) => { const el = row.querySelector('[data-upload-error]'); el.hidden = false; el.textContent = attachmentError(error); };
      row.querySelector('[data-upload-resume]')?.addEventListener('change', (event) => { const file = event.detail?.files?.[0]; if (file) startUpload(file, record.uploadId); });
      row.querySelector('[data-upload-status]').addEventListener('click', async (event) => {
        const button = event.currentTarget; button.setAttribute('disabled', '');
        try { await refreshAttachmentUpload(record); renderPending(); } catch (error) { fail(error); button.removeAttribute('disabled'); }
      });
      row.querySelector('[data-upload-cancel]').addEventListener('click', async (event) => {
        const button = event.currentTarget; button.setAttribute('disabled', '');
        try { await cancelAttachmentUpload(pid, record.uploadId); renderPending(); } catch (error) { fail(error); button.removeAttribute('disabled'); }
      });
      row.querySelector('[data-upload-recover]')?.addEventListener('click', async (event) => {
        event.currentTarget.setAttribute('disabled', '');
        try {
          const current = await refreshAttachmentUpload(record);
          if (!current.complete) throw new Error(t('attachment_upload_incomplete'));
          onAdded({ sha256: current.sha256, name: current.filename, size_bytes: current.totalSize, mime: current.mime }); renderPending();
        } catch (error) { fail(error); event.currentTarget.removeAttribute('disabled'); }
      });
      host.appendChild(row);
    }
  };
  const startUpload = async (file, resumeUploadId = null) => {
    if (closed) return;
    const controller = new AbortController();
    const key = resumeUploadId || crypto.randomUUID();
    const row = document.createElement('div'); row.className = 'ps-upload-row'; row.dataset.activeUpload = key;
    row.innerHTML = `<b>${escapeHtml(file.name)}</b><tf-progress-bar value="0"></tf-progress-bar><div class="ps-task-comment-tools"><span data-upload-state></span><tf-button variant="ghost" data-upload-pause>${escapeHtml(t('attachment_upload_pause'))}</tf-button></div>`;
    host.appendChild(row);
    const operation = { controller, row, uploadId: resumeUploadId };
    active.set(key, operation); syncSave();
    renderPending();
    row.querySelector('[data-upload-pause]').addEventListener('click', () => { controller.abort(); row.querySelector('[data-upload-pause]').setAttribute('disabled', ''); });
    try {
      const attachment = await uploadAttachmentFile(file, { signal: controller.signal, resumeUploadId,
        onProgress: (progress) => {
          if (closed) return;
          if (progress.uploadId && operation.uploadId !== progress.uploadId) { operation.uploadId = progress.uploadId; renderPending(); }
          const percent = progress.total ? Math.floor(progress.offset / progress.total * 100) : progress.phase === 'complete' ? 100 : 0;
          const bar = row.querySelector('tf-progress-bar');
          bar.setAttribute('value', String(percent)); bar.setAttribute('label', t(`attachment_upload_${progress.phase}`, { percent }));
          bar.setAttribute('role', 'progressbar'); bar.setAttribute('aria-valuenow', String(percent)); bar.setAttribute('aria-valuemin', '0'); bar.setAttribute('aria-valuemax', '100'); bar.setAttribute('aria-label', `${file.name}: ${t(`attachment_upload_${progress.phase}`, { percent })}`);
          row.querySelector('[data-upload-state]').textContent = `${formatBytes(progress.offset)} / ${formatBytes(progress.total)}`;
        },
      });
      if (!closed) onAdded(attachment);
    } catch (error) { if (error.name !== 'AbortError') showError(error); }
    finally { active.delete(key); if (!closed) { row.remove(); syncSave(); renderPending(); } }
  };
  body.querySelector('#ps-task-att-input').addEventListener('change', async (event) => {
    for (const file of event.detail?.files || []) { if (closed) break; await startUpload(file); }
  });
  body.addEventListener('paste', async (event) => {
    const images = [...(event.clipboardData?.items || [])].filter((item) => item.kind === 'file' && item.type.startsWith('image/'));
    if (!images.length) return;
    event.preventDefault();
    for (const item of images) {
      const source = item.getAsFile();
      if (!source) continue;
      const extension = source.type.split('/')[1].replace(/[^a-z0-9]/g, '') || 'png';
      const file = new File([source], `clipboard-${new Date().toISOString().replace(/[:.]/g, '-')}.${extension}`, { type: source.type });
      await startUpload(file);
    }
  });
  const close = () => { if (closed) return; closed = true; for (const operation of active.values()) operation.controller.abort(); };
  signal.addEventListener('abort', close);
  renderPending();
  return { get busy() { return active.size > 0; }, close };
}

async function openTaskWindow(opts = {}) {
  if (opts.taskId ? !(canArea('tasks') || canArea('board')) : !canCreateTask(projectAccess())) return;
  const pid = projectId();
  const tw = {
    taskId: opts.taskId || null, taskType: opts.taskType || 'technical', title: opts.titlePrefill || '',
    descriptionMd: '', severity: '', priority: 'medium', status: TASK_STATUSES.includes(opts.status) ? opts.status : 'todo',
    assignedTo: '', dueDate: '', parentTaskId: opts.parentTaskId || null,
    links: Array.isArray(opts.links) ? opts.links.slice() : [], taskLinks: [],
    attachments: [], comments: [], events: [], eventsHasMore: false, durations: [], handoverCommentId: null, info: null, busy: false,
    ...opts.draft,
  };
  await ensureF2Members();
  const readDetail = async () => {
    const response = await ApiBinary.one('projectStudioTaskGetRequest', { projectId: pid, taskId: tw.taskId });
    const detail = response.detail;
    tw.info = detail.info;
    tw.comments = detail.comments;
    tw.events = detail.events;
    tw.eventsHasMore = detail.events_has_more;
    tw.durations = detail.status_durations;
    tw.taskLinks = detail.task_links;
    tw.handoverCommentId = detail.handover_comment_id;
    return detail;
  };
  try {
    await loadTaskTypes();
    if (!tw.taskId && !state.taskTypes.some((type) => type.active && type.type_id === tw.taskType)) tw.taskType = state.taskTypes.find((type) => type.active)?.type_id || '';
    if (tw.taskId) {
      const detail = await readDetail();
      const info = detail.info;
      tw.taskType = info.task_type;
      tw.title = info.title;
      tw.descriptionMd = detail.description_md;
      tw.severity = info.severity;
      tw.priority = info.priority;
      tw.status = info.status;
      tw.assignedTo = info.assigned_to;
      tw.dueDate = info.due_date;
      tw.parentTaskId = info.parent_task_id;
      tw.links = JSON.parse(info.links_json);
      tw.attachments = detail.attachments.slice();
      while (opts.eventId && !tw.events.some((event) => String(event.event_id) === String(opts.eventId)) && tw.eventsHasMore) {
        const page = await ApiBinary.one('projectStudioTaskEventsRequest', { projectId: pid, taskId: tw.taskId, beforeId: tw.events.at(-1).event_id, limit: 50 });
        tw.events.push(...page.events); tw.eventsHasMore = page.has_more;
      }
    }
  } catch (error) {
    toast(`${t('task_load_failed')}: ${error.message}`, 'error');
    return;
  }
  const archived = !!tw.info?.archived_at;
  const mayEdit = !archived && (tw.taskId ? canArea('tasks', 'write') : canCreateTask(projectAccess()));
  const maySetStatus = !archived && !!tw.taskId && (canArea('tasks', 'write') || canArea('board', 'write'));
  const mayArchive = !!tw.taskId && canArea('tasks', 'write');
  const mayReadTasks = canArea('tasks') || canArea('board');
  const mayComment = !archived && !!tw.taskId && canArea('tasks', 'write');
  const mayHandover = !projectAccess().archived && !projectAccess().ended && !archived && tw.status !== 'done' && !!tw.taskId && (canArea('tasks', 'write') || isMe(tw.assignedTo));
  const types = activeTaskTypes(state.taskTypes, tw.taskType);
  const members = (f2().membersCache || []).filter((member) => member.access.has_access);
  const assignees = members.filter((member) => allowsArea(member.access, 'tasks', 'write'));
  const readers = members.filter((member) => allowsArea(member.access, 'tasks') || allowsArea(member.access, 'board'));
  const { body, foot, cleanup, signal } = openWindow({
    title: tw.taskId ? t('task_win_title', { no: taskNoLabel(tw.info) }) : t('task_win_new_title'),
    subtitle: tw.info ? `${fv(tw.info, 'created_by_name')} · ${formatTimestamp(fv(tw.info, 'created_at'))}` : t('task_new_key_hint', { prefix: fv(state.project, 'key_prefix') }),
    icon: tw.taskType === 'defect' ? 'alert' : 'check', width: 1080,
  });
  body.classList.add('ps-task-card');
  if (tw.info) updateProjectRoute({ taskKey: tw.info.task_key, taskId: null, eventId: opts.eventId || null, commentId: opts.commentId || null });
  body.innerHTML = `
    ${tw.info?.resolution === 'not_pursued' ? `<div class="ps-task-resolution"><tf-chip status="info">${escapeHtml(t('task_not_pursued'))}</tf-chip><span>${escapeHtml(tw.info.resolution_reason)}</span></div>` : ''}
    ${archived ? `<div class="ps-task-archived"><tf-chip status="warn">${escapeHtml(t('task_archived'))}</tf-chip><span>${escapeHtml(t('task_archived_hint'))}</span></div>` : ''}
    <div class="ps-task-layout">
      <div class="ps-task-main">
        <div id="ps-task-handover-note" hidden></div>
        <tf-input id="ps-task-title" label="${escapeAttr(t('task_title_label'))}" value="${escapeAttr(tw.title)}" ${mayEdit ? '' : 'readonly'}></tf-input>
        <tf-section-card title="${escapeAttr(t('task_desc_label'))}" icon="file-text">
          ${mayEdit ? `<tf-segmented id="ps-task-desc-mode" value="edit"><option value="edit">${escapeHtml(t('action_edit'))}</option><option value="preview">${escapeHtml(t('task_description_preview'))}</option></tf-segmented><tf-textarea id="ps-task-desc" rows="6" autogrow hint="${escapeAttr(t('task_desc_hint'))}"></tf-textarea>` : ''}
          <div id="ps-task-desc-preview" class="ps-task-markdown" ${mayEdit ? 'hidden' : ''}></div>
        </tf-section-card>
        <tf-section-card title="${escapeAttr(t('case_attachments_title'))}" icon="paperclip">
          <div id="ps-task-atts" class="ps-task-gallery"></div>
          ${mayEdit ? `<tf-file-input id="ps-task-att-input" multiple label="${escapeAttr(t('attachment_dropzone'))}"></tf-file-input><div id="ps-task-uploads"></div><span class="ps-field-hint">${escapeHtml(t('attachment_clipboard_hint'))}</span>` : ''}
        </tf-section-card>
        ${tw.taskId ? `<tf-section-card title="${escapeAttr(t('task_comments_title'))}" icon="message">
          <div class="ps-task-comments" id="ps-task-comments"></div>
          ${mayComment ? `<div class="ps-task-comment-composer">
            <tf-textarea id="ps-task-comment-input" rows="3" autogrow label="${escapeAttr(t('task_comment_placeholder'))}"></tf-textarea>
            <tf-person-picker id="ps-task-mentions" multiple label="${escapeAttr(t('task_mentions_label'))}" hidden></tf-person-picker>
            <div class="ps-task-comment-tools"><tf-button variant="ghost" icon="users" id="ps-task-mention-toggle">${escapeHtml(t('task_mentions_add'))}</tf-button><span id="ps-task-mention-summary"></span><tf-button variant="primary" icon="send" id="ps-task-comment-send">${escapeHtml(t('task_comment_send'))}</tf-button></div>
          </div>` : ''}
        </tf-section-card>` : ''}
      </div>
      <aside class="ps-task-sidebar">
        <tf-section-card title="${escapeAttr(t('task_metadata_title'))}" icon="settings">
          <div class="ps-task-metadata">
            ${!tw.taskId ? `<tf-select id="ps-task-project" label="${escapeAttr(t('task_source_project'))}" value="${escapeAttr(pid)}">${descendantsOf(pid).filter((project) => canCreateTask(project.access)).map((project) => `<option value="${escapeAttr(project.project_id)}" ${project.project_id === pid ? 'selected' : ''}>${escapeHtml(project.name)}</option>`).join('')}</tf-select><span class="ps-field-hint" data-task-project-hint hidden>${escapeHtml(t('task_project_change_upload_hint'))}</span>` : `<span class="ps-field-hint">${escapeHtml(state.project.name)}</span>`}
            <tf-select id="ps-task-type" label="${escapeAttr(t('task_type_label'))}" value="${escapeAttr(tw.taskType)}" ${mayEdit ? '' : 'disabled'}>
              ${types.map((type) => `<option value="${escapeAttr(type.type_id)}" ${type.type_id === tw.taskType ? 'selected' : ''} ${!type.active && type.type_id !== tw.taskType || type.type_id === 'subtask' && !mayReadTasks ? 'disabled' : ''}>${escapeHtml(taskTypeLabel(type, t))}${type.active ? '' : ` · ${escapeHtml(t('task_type_inactive'))}`}</option>`).join('')}
            </tf-select>
            <span id="ps-task-type-hint" class="ps-field-hint"></span>
            <tf-select id="ps-task-status" label="${escapeAttr(t('task_status_label'))}" value="${escapeAttr(tw.status)}" ${mayEdit || maySetStatus ? '' : 'disabled'}>
              ${TASK_STATUSES.map((status) => selectOpt(status, tw.status, t(`task_status_${status}`))).join('')}
            </tf-select>
            <tf-select id="ps-task-severity" label="${escapeAttr(t('task_severity_label'))}" value="${escapeAttr(tw.severity)}" ${mayEdit ? '' : 'disabled'}>
              ${selectOpt('', tw.severity, '—')}${PRIORITIES.map((priority) => selectOpt(priority, tw.severity, t(`sev_${priority}`))).join('')}
            </tf-select>
            <tf-select id="ps-task-priority" label="${escapeAttr(t('task_priority_label'))}" value="${escapeAttr(tw.priority)}" ${mayEdit ? '' : 'disabled'}>
              ${PRIORITIES.map((priority) => selectOpt(priority, tw.priority, t(`prio_${priority}`))).join('')}
            </tf-select>
            <tf-select id="ps-task-assignee" label="${escapeAttr(t('task_assignee_label'))}" value="${escapeAttr(tw.assignedTo)}" ${mayEdit ? '' : 'disabled'}>
              ${selectOpt('', tw.assignedTo, t('task_unassigned'))}${assignees.map((member) => selectOpt(member.user_id, tw.assignedTo, member.display_name)).join('')}
            </tf-select>
            ${mayHandover ? `<tf-button variant="ghost" icon="send" id="ps-task-handover">${escapeHtml(t('task_handover'))}</tf-button>` : ''}
            <tf-input id="ps-task-due" type="date" label="${escapeAttr(t('task_due_label'))}" value="${escapeAttr(tw.dueDate)}" ${mayEdit ? '' : 'readonly'}></tf-input>
            ${mayReadTasks ? `<tf-combobox id="ps-task-parent" label="${escapeAttr(t('task_parent_label'))}" placeholder="${escapeAttr(t('task_parent_search'))}" clearable ${mayEdit ? '' : 'disabled'}></tf-combobox><span class="ps-field-hint">${escapeHtml(t('task_parent_hint'))}</span>` : ''}
          </div>
        </tf-section-card>
        <tf-section-card title="${escapeAttr(t('task_relations_title'))}" icon="branch">
          <div id="ps-task-relations"></div>
          ${mayEdit && tw.taskId ? `<tf-button variant="ghost" icon="plus" id="ps-task-link-add">${escapeHtml(t('task_relation_add'))}</tf-button>` : ''}
          ${!tw.taskId ? `<div class="ps-field-hint">${escapeHtml(t('task_relation_save_first'))}</div>` : ''}
          <div class="ps-task-links" id="ps-task-links"></div>
        </tf-section-card>
        ${tw.taskId ? `<tf-section-card title="${escapeAttr(t('task_status_time_title'))}" icon="clock"><div id="ps-task-durations"></div></tf-section-card>
          <tf-section-card title="${escapeAttr(t('task_history_title'))}" icon="list"><tf-timeline id="ps-task-history"></tf-timeline><tf-button variant="ghost" id="ps-task-history-more">${escapeHtml(t('task_history_more'))}</tf-button></tf-section-card>` : ''}
      </aside>
    </div>
    <div class="ps-form-error" data-form-error hidden></div>`;
  foot.innerHTML = `<div class="ps-footer-left">${tw.info && projectAccess().is_owner && !projectAccess().archived && !projectAccess().ended && !archived ? `<tf-button variant="ghost" icon="branch" data-action="transfer">${escapeHtml(t('task_transfer'))}</tf-button>` : ''}${mayEdit && tw.taskId && tw.status !== 'done' ? `<tf-button variant="ghost" icon="check" data-action="resolve">${escapeHtml(t('task_not_pursued'))}</tf-button>` : ''}${mayArchive ? `<tf-button variant="ghost" icon="${archived ? 'refresh' : 'archive'}" data-action="archive">${escapeHtml(t(archived ? 'task_restore' : 'action_archive'))}</tf-button>` : ''}${tw.info ? `<tf-button variant="ghost" icon="link" data-action="copy-link">${escapeHtml(t('task_copy_link'))}</tf-button>` : ''}</div>
    <div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_close'))}</tf-button>${mayEdit || maySetStatus ? `<tf-button variant="primary" icon="check" data-action="save">${escapeHtml(t('action_save'))}</tf-button>` : ''}</div>`;
  const errorEl = body.querySelector('[data-form-error]');
  const showError = (message) => { errorEl.hidden = !message; errorEl.textContent = message || ''; };
  const description = body.querySelector('#ps-task-desc');
  if (description) description.value = tw.descriptionMd;
  const renderDescription = () => { body.querySelector('#ps-task-desc-preview').innerHTML = renderMarkdown(tw.descriptionMd) || `<span class="ps-field-hint">${escapeHtml(t('task_description_empty'))}</span>`; };
  renderDescription();
  body.querySelector('#ps-task-desc-mode')?.addEventListener('change', (event) => {
    const preview = event.detail?.value === 'preview';
    description.hidden = preview;
    body.querySelector('#ps-task-desc-preview').hidden = !preview;
    if (preview) renderDescription();
  });

  const historyTaskNames = new Map();
  const historyTaskRequests = new Set();
  const historyProjectNames = new Map([[pid, state.project.name]]);
  const historyProjectRequests = new Set();
  const rememberHistoryTask = (task) => { historyTaskNames.set(task.task_id, `${task.task_key} · ${task.title}`); historyProjectNames.set(task.project_id, task.project_name); };
  for (const task of [...(state.tasksView?.rows || []), ...(state.tasksView?.boardRows || [])]) rememberHistoryTask(task);
  if (tw.info) rememberHistoryTask(tw.info);
  for (const link of tw.taskLinks) historyTaskNames.set(link.source_task_id === tw.taskId ? link.target_task_id : link.source_task_id, `${link.counterparty_task_key} · ${link.counterparty_task_title}`);
  let renderHistory;
  const parent = body.querySelector('#ps-task-parent');
  let parentQuery = 0;
  let parentRows = [];
  const findParents = async (search = '') => {
    if (!parent || !mayEdit || tw.taskType === 'epic') return;
    const query = ++parentQuery;
    const response = await ApiBinary.one('projectStudioTasksListRequest', { projectId: pid, scope: 'single', sourceProjectIds: [], taskType: tw.taskType === 'subtask' ? '' : 'epic', status: '', assignedTo: '', search, severity: '', offset: 0, limit: 50 });
    if (!parent.isConnected || query !== parentQuery) return;
    parentRows = parentCandidates(response.tasks, tw.taskType, tw.taskId);
    for (const task of parentRows) rememberHistoryTask(task);
    parent.options = parentRows.map((task) => ({ value: task.task_id, label: `${task.task_key} · ${task.title}` }));
    renderHistory?.();
  };
  const syncType = () => {
    const type = state.taskTypes.find((entry) => entry.type_id === tw.taskType);
    body.querySelector('#ps-task-type-hint').textContent = type ? taskTypeDescription(type, t) : '';
    body.querySelector('#ps-task-severity').hidden = tw.taskType !== 'defect';
    if (parent) { parent.disabled = !mayEdit || tw.taskType === 'epic'; parent.setAttribute('label', t(tw.taskType === 'subtask' ? 'task_parent_required' : 'task_parent_label')); }
  };
  syncType();
  parent?.addEventListener('input', (event) => findParents(event.detail?.query || '').catch((error) => showError(`${t('task_load_failed')}: ${error.message}`)));
  parent?.addEventListener('change', (event) => {
    tw.parentTaskId = !event.detail?.free && parent.options.some((option) => option.value === event.detail?.value) ? event.detail.value : null;
  });
  if (tw.parentTaskId && parent) {
    try {
      const response = await ApiBinary.one('projectStudioTaskGetRequest', { projectId: pid, taskId: tw.parentTaskId });
      parentRows.push(response.detail.info);
      rememberHistoryTask(response.detail.info);
      parent.value = `${response.detail.info.task_key} · ${response.detail.info.title}`;
    } catch (error) { historyTaskNames.set(tw.parentTaskId, null); showError(`${t('task_load_failed')}: ${error.message}`); }
  }
  if (parent && mayEdit) findParents().catch((error) => showError(`${t('task_load_failed')}: ${error.message}`));

  const renderLinks = () => {
    body.querySelector('#ps-task-links').innerHTML = tw.links.map((link, index) => `<tf-chip status="accent" ${mayEdit ? 'removable' : ''} data-task-link="${index}">${escapeHtml(t(`link_kind_${link.kind}`))}: ${escapeHtml(link.label || link.id)}</tf-chip>`).join('');
    body.querySelectorAll('[data-task-link]').forEach((chip) => chip.addEventListener('remove', () => { tw.links.splice(Number(chip.dataset.taskLink), 1); renderLinks(); }));
    const host = body.querySelector('#ps-task-relations');
    host.innerHTML = tw.taskLinks.length ? tw.taskLinks.map((link) => {
      const otherId = link.source_task_id === tw.taskId ? link.target_task_id : link.source_task_id;
      const direction = ['related', 'duplicate'].includes(link.kind) ? '' : t(link.source_task_id === tw.taskId ? 'task_relation_outgoing' : 'task_relation_incoming');
      return `<div class="ps-task-relation"><tf-button variant="ghost" data-related-task="${escapeAttr(otherId)}" data-related-project="${escapeAttr(link.counterparty_project_id)}">${escapeHtml(link.counterparty_task_key)} · ${escapeHtml(link.counterparty_task_title)}</tf-button><span>${escapeHtml(t(`task_relation_${link.kind}`))}${direction ? ` · ${escapeHtml(direction)}` : ''}${link.lag_days ? ` · ${escapeHtml(t('task_relation_lag_value', { days: link.lag_days }))}` : ''}</span>${mayEdit ? `<tf-button variant="ghost" size="sm" icon="x" data-unlink="${link.link_id}" title="${escapeAttr(t('task_relation_remove'))}"></tf-button>` : ''}</div>`;
    }).join('') : `<span class="ps-field-hint">${escapeHtml(t('task_relations_empty'))}</span>`;
    host.querySelectorAll('[data-related-task]').forEach((button) => button.addEventListener('click', () => { cleanup(); openProject(button.dataset.relatedProject, { tab: 'tasks', taskId: button.dataset.relatedTask }); }));
    host.querySelectorAll('[data-unlink]').forEach((button) => button.addEventListener('click', async () => {
      button.setAttribute('disabled', '');
      try { await ApiBinary.one('projectStudioTaskLinkDeleteRequest', { projectId: pid, linkId: Number(button.dataset.unlink) }); await refreshRecorded(); }
      catch (error) { button.removeAttribute('disabled'); showError(`${t('task_relation_failed')}: ${error.message}`); }
    }));
  };
  body.querySelector('#ps-task-link-add')?.addEventListener('click', () => openTaskLinkWindow(tw.taskId, refreshRecorded));

  renderHistory = () => {
    const history = body.querySelector('#ps-task-history');
    if (!history) return;
    const markedId = history.querySelector('.ps-task-target [data-task-event]')?.dataset.taskEvent;
    const eventValue = (json, kind) => taskEventValue(json, { translate: t, memberName, taskName: (id) => historyTaskNames.get(id), projectName: (id) => historyProjectNames.get(id), typeName: taskTypeName, kind });
    history.entries = tw.events.map((event) => ({
      title: `<span data-task-event="${event.event_id}">${escapeHtml(t(`task_event_${event.kind}`))}</span>`, time: escapeHtml(formatTimestamp(event.at)),
      description: `<span class="ps-task-history-actor">${escapeHtml(event.actor_kind === 'user' ? memberName(event.actor_id) || t('task_history_unavailable_person') : t(`task_actor_${event.actor_kind}`))}</span><div class="ps-task-history-values">${escapeHtml(eventValue(event.before_json, event.kind))}<span aria-hidden="true"> → </span>${escapeHtml(eventValue(event.after_json, event.kind))}</div>${taskEventAttachments(event).map((attachment, index) => `<tf-button variant="ghost" size="sm" icon="paperclip" data-history-attachment="${event.event_id}:${index}">${escapeHtml(attachment.name)}</tf-button>`).join('')}`,
    }));
    if (markedId) {
      const marked = [...history.querySelectorAll('[data-task-event]')].find((element) => element.dataset.taskEvent === markedId)?.closest('.tf-timeline-item');
      marked?.classList.add('ps-task-target'); marked?.setAttribute('tabindex', '-1');
    }
    history.querySelectorAll('[data-history-attachment]').forEach((button) => button.addEventListener('click', () => {
      const [eventId, index] = button.dataset.historyAttachment.split(':').map(Number);
      const event = tw.events.find((item) => Number(item.event_id) === eventId);
      openAttachmentPreview(taskEventAttachments(event)[index], attachmentOwner('task', tw.taskId, null, pid));
    }));
    const more = body.querySelector('#ps-task-history-more');
    more.hidden = !tw.eventsHasMore;
    body.querySelector('#ps-task-durations').innerHTML = tw.durations.map((duration) => `<div class="ps-task-duration"><tf-chip status="${TASK_STATUS_CHIP[duration.status]}">${escapeHtml(t(`task_status_${duration.status}`))}</tf-chip><b>${escapeHtml(taskDuration(duration.seconds, t))}</b><span>${escapeHtml(formatTimestamp(duration.entered_at))}${duration.left_at ? ` → ${escapeHtml(formatTimestamp(duration.left_at))}` : ` · ${escapeHtml(t('task_status_current'))}`}</span></div>`).join('');
  };
  const resolveHistoryTasks = async (events) => {
    if (!mayReadTasks || !tw.taskId) return;
    const references = events.map(taskEventReferences);
    const jobs = [];
    for (const id of new Set(references.flatMap((entry) => entry.tasks))) {
      if (historyTaskNames.has(id) || historyTaskRequests.has(id)) continue;
      historyTaskRequests.add(id); jobs.push({ kind: 'task', id });
    }
    for (const id of new Set(references.flatMap((entry) => entry.projects))) {
      if (historyProjectNames.has(id) || historyProjectRequests.has(id)) continue;
      historyProjectRequests.add(id); jobs.push({ kind: 'project', id });
    }
    for (let i = 0; i < jobs.length && !signal.aborted; i += 5) {
      await Promise.all(jobs.slice(i, i + 5).map(async ({ kind, id }) => {
        try {
          if (kind === 'project') {
            const response = await ApiBinary.one('projectStudioProjectGetRequest', { projectId: id });
            if (!signal.aborted) historyProjectNames.set(id, response.project.name);
          } else {
            const target = await ApiBinary.one('projectStudioTaskKeyResolveRequest', { taskKey: null, taskId: id, originProjectId: null, originEventId: null });
            const response = await ApiBinary.one('projectStudioTaskGetRequest', { projectId: target.project_id, taskId: target.task_id });
            if (!signal.aborted) rememberHistoryTask(response.detail.info);
          }
        } catch { (kind === 'project' ? historyProjectNames : historyTaskNames).set(id, null); }
      }));
      if (!signal.aborted && body.isConnected) renderHistory();
    }
  };
  body.querySelector('#ps-task-history-more')?.addEventListener('click', async (event) => {
    const button = event.currentTarget;
    button.setAttribute('disabled', '');
    try {
      const response = await ApiBinary.one('projectStudioTaskEventsRequest', { projectId: pid, taskId: tw.taskId, beforeId: tw.events.at(-1)?.event_id ?? null, limit: 50 });
      tw.events.push(...response.events);
      tw.eventsHasMore = response.has_more;
      renderHistory();
      await resolveHistoryTasks(response.events);
      button.hidden = !response.has_more;
    } catch (error) { showError(`${t('task_load_failed')}: ${error.message}`); }
    finally { button.removeAttribute('disabled'); }
  });

  let galleryCleanup = null;
  const renderAtts = () => {
    galleryCleanup?.();
    const host = body.querySelector('#ps-task-atts');
    galleryCleanup = renderTaskAttachmentGallery(host, tw.attachments, attachmentOwner('task', tw.taskId, null, pid), { removable: mayEdit, onRemove: (index) => { tw.attachments.splice(index, 1); renderAtts(); } });
  };
  const renderComments = () => {
    const host = body.querySelector('#ps-task-comments');
    if (!host) return;
    const pin = body.querySelector('#ps-task-handover-note');
    const note = isMe(tw.info?.assigned_to) && tw.handoverCommentId ? tw.comments.find((comment) => comment.comment_id === tw.handoverCommentId) : null;
    pin.hidden = !note;
    pin.innerHTML = note ? `<tf-section-card title="${escapeAttr(t('task_handover_note'))}" icon="pin"><div class="ps-task-handover-note"><b>${escapeHtml(note.author_name)}</b><span>${escapeHtml(formatTimestamp(note.created_at))}</span><div class="ps-task-markdown">${renderMarkdown(note.body_md)}</div></div></tf-section-card>` : '';
    host.innerHTML = tw.comments.length ? tw.comments.map((comment, index) => {
      const own = isMe(comment.author_user_id);
      return `<div class="ps-task-comment" data-task-comment="${escapeAttr(comment.comment_id)}"><div class="ps-av-mini">${escapeHtml(initials(comment.author_name))}</div><div class="ps-task-comment-main"><div class="ps-task-comment-head"><b>${escapeHtml(comment.author_name)}</b><span>${escapeHtml(formatTimestamp(comment.created_at))}${comment.edited_at ? ` · ${escapeHtml(t('task_comment_edited'))}` : ''}</span></div><div class="ps-task-markdown">${renderMarkdown(comment.body_md)}</div><div class="ps-task-mentioned">${comment.mention_user_ids.map((id) => `<tf-chip status="accent">@${escapeHtml(memberName(id) || id)}</tf-chip>`).join('')}</div></div><div class="ps-task-comment-actions">${mayComment && own ? `<tf-button variant="ghost" size="sm" icon="edit" data-comment-edit="${index}" title="${escapeAttr(t('action_edit'))}"></tf-button>` : ''}${mayComment && (own || canArea('tasks', 'admin')) ? `<tf-button variant="ghost" size="sm" icon="trash" data-comment-delete="${index}" title="${escapeAttr(t('action_delete'))}"></tf-button>` : ''}</div></div>`;
    }).join('') : `<span class="ps-field-hint">${escapeHtml(t('task_comments_empty'))}</span>`;
    host.querySelectorAll('[data-comment-edit]').forEach((button) => button.addEventListener('click', () => openTaskCommentEditor(tw.comments[Number(button.dataset.commentEdit)], readers, refreshRecorded)));
    host.querySelectorAll('[data-comment-delete]').forEach((button) => button.addEventListener('click', async () => {
      const comment = tw.comments[Number(button.dataset.commentDelete)];
      if (!await TfWindow.confirm({ title: t('task_comment_delete_title'), message: t('task_comment_delete_message'), confirmLabel: t('action_delete'), cancelLabel: t('action_cancel'), danger: true })) return;
      try { await ApiBinary.one('projectStudioTaskCommentDeleteRequest', { projectId: pid, commentId: comment.comment_id }); await refreshRecorded(); }
      catch (error) { showError(`${t('task_comment_failed')}: ${error.message}`); }
    }));
  };
  async function refreshRecorded() {
    await readDetail();
    if (!body.isConnected) return;
    rememberHistoryTask(tw.info);
    renderLinks(); renderComments(); renderHistory();
    await resolveHistoryTasks(tw.events.slice(0, 50));
  }
  body.querySelector('#ps-task-handover')?.addEventListener('click', (event) => {
    const win = openHandoverWindow({ subject: `${tw.info.task_key} · ${tw.title}`, title: t('task_handover'), submitLabel: t('task_handover'), anchor: event.currentTarget,
      note: { tone: 'info', text: t('task_handover_hint') },
      people: assignees.filter((member) => member.user_id !== tw.info.assigned_to).map((member) => ({ id: member.user_id, name: member.display_name })),
      onSubmit: async ({ personId, comment }) => {
        await ApiBinary.one('projectStudioTaskHandoverRequest', { projectId: pid, taskId: tw.taskId, assignedTo: personId, noteMd: comment, mentionUserIds: [personId] });
        cleanup();
        if (projectId() === pid && state.tab === 'tasks') await renderTasksTab();
        return { message: t('task_handover_saved') };
      }, errorMessage: (error) => `${t('task_handover_failed')}: ${error.message}`,
    });
    const closeHandover = () => { win.close(true); state.wins.delete(closeHandover); };
    state.wins.add(closeHandover);
    win.addEventListener('close-request', () => state.wins.delete(closeHandover));
  });
  const mentionPicker = body.querySelector('#ps-task-mentions');
  if (mentionPicker) {
    mentionPicker.items = readers.map((member) => ({ id: member.user_id, name: member.display_name }));
    body.querySelector('#ps-task-mention-toggle').addEventListener('click', () => { mentionPicker.hidden = !mentionPicker.hidden; if (!mentionPicker.hidden) mentionPicker.focusSearch(); });
    mentionPicker.addEventListener('change', () => { body.querySelector('#ps-task-mention-summary').textContent = mentionPicker.selectedItems.map((member) => `@${member.name}`).join(', '); });
  }
  body.querySelector('#ps-task-comment-send')?.addEventListener('click', async (event) => {
    const input = body.querySelector('#ps-task-comment-input');
    const bodyMd = input.value.trim();
    if (!bodyMd) return;
    const button = event.currentTarget;
    button.setAttribute('disabled', '');
    try {
      await ApiBinary.one('projectStudioTaskCommentAddRequest', { projectId: pid, taskId: tw.taskId, bodyMd, mentionUserIds: mentionPicker.value });
      input.value = ''; mentionPicker.value = []; body.querySelector('#ps-task-mention-summary').textContent = '';
      await refreshRecorded();
    } catch (error) { showError(`${t('task_comment_failed')}: ${error.message}`); }
    finally { button.removeAttribute('disabled'); }
  });

  body.querySelector('#ps-task-type').addEventListener('change', (event) => {
    tw.taskType = event.detail?.value ?? tw.taskType;
    if (tw.taskType === 'epic') { tw.parentTaskId = null; if (parent) parent.value = ''; }
    syncType(); if (parent && mayEdit) findParents().catch((error) => showError(error.message));
  });
  body.querySelector('#ps-task-title').addEventListener('input', (event) => { tw.title = String(event.target.value ?? ''); });
  description?.addEventListener('input', () => { tw.descriptionMd = description.value; });
  for (const [id, field] of [['severity', 'severity'], ['priority', 'priority'], ['status', 'status'], ['assignee', 'assignedTo']]) body.querySelector(`#ps-task-${id}`).addEventListener('change', (event) => { tw[field] = event.detail?.value ?? ''; });
  body.querySelector('#ps-task-due').addEventListener('change', (event) => { tw.dueDate = String(event.target.value ?? ''); });
  const uploads = wireTaskUploads({ body, foot, signal, attachments: tw.attachments, pid, onAdded: (attachment) => { if (!tw.attachments.some((entry) => entry.sha256 === attachment.sha256)) tw.attachments.push(attachment); renderAtts(); } });
  body.querySelector('#ps-task-project')?.addEventListener('change', async (event) => {
    const destinationId = event.detail.value;
    if (destinationId === pid) return;
    if (tw.attachments.length || uploads.busy) { body.querySelector('#ps-task-project').value = pid; body.querySelector('[data-task-project-hint]').hidden = false; return; }
    const destination = descendantsOf(pid).find((project) => project.project_id === destinationId && canCreateTask(project.access));
    if (!destination) return;
    const draft = { taskType: tw.taskType, title: tw.title, descriptionMd: tw.descriptionMd, severity: tw.severity, priority: tw.priority, status: tw.status, dueDate: tw.dueDate };
    cleanup(); await switchProject(destinationId); await openTaskWindow({ draft });
  });
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]');
    if (!button || button.hasAttribute('disabled') || tw.busy || uploads.busy && button.dataset.action !== 'cancel') return;
    if (button.dataset.action === 'cancel') { uploads.close(); galleryCleanup?.(); cleanup(); return; }
    if (button.dataset.action === 'copy-link') {
      try { await navigator.clipboard.writeText(location.href); toast(t('task_link_copied'), 'success'); }
      catch (error) { showError(`${t('task_link_copy_failed')}: ${error.message}`); }
      return;
    }
    if (button.dataset.action === 'transfer') { openTaskTransferWindow(tw.info, state.project, async (result) => { cleanup(); await openProject(result.destination_project_id, { tab: 'tasks', taskId: result.task_id }); }); return; }
    if (button.dataset.action === 'resolve') { openTaskResolutionWindow(tw.info, state.project, async () => { cleanup(); await renderTasksTab(); }); return; }
    if (button.dataset.action === 'archive') {
      try { await setTaskArchived(tw.taskId, !archived, pid); cleanup(); }
      catch (error) { showError(`${t('task_archive_failed')}: ${error.message}`); }
      return;
    }
    const title = tw.title.trim();
    if (mayEdit && title.length < 3) { showError(t('err_task_title')); return; }
    if (mayEdit && tw.taskType === 'defect' && !tw.severity) { showError(t('err_task_severity')); return; }
    if (mayEdit && tw.taskType === 'subtask' && !tw.parentTaskId) { showError(t('err_task_parent')); return; }
    tw.busy = true; button.setAttribute('disabled', '');
    try {
      let response;
      if (!mayEdit && maySetStatus) {
        response = await ApiBinary.one('projectStudioTaskStatusSetRequest', { projectId: pid, taskId: tw.taskId, status: tw.status });
      } else {
        response = await ApiBinary.one('projectStudioTaskSaveRequest', { projectId: pid, taskId: tw.taskId, taskType: tw.taskType, title, descriptionMd: tw.descriptionMd,
          severity: tw.taskType === 'defect' ? tw.severity : '', priority: tw.priority, status: tw.status, assignedTo: tw.assignedTo, dueDate: tw.dueDate,
          parentTaskId: tw.parentTaskId, linksJson: JSON.stringify(tw.links), attachmentsJson: JSON.stringify(tw.attachments) });
      }
      toast(t('task_saved', { no: response.task_key || tw.info?.task_key }), 'success');
      acknowledgeAttachmentUploads(pid, currentUserId(), tw.attachments);
      uploads.close(); galleryCleanup?.();
      cleanup();
      if (state.tab === 'tasks' && mayReadTasks) await renderTasksTab();
    } catch (error) { tw.busy = false; button.removeAttribute('disabled'); showError(`${t('task_save_failed')}: ${error.message}`); }
  });
  renderLinks(); renderAtts(); renderComments(); renderHistory();
  resolveHistoryTasks(tw.events.slice(0, 50)).catch((error) => showError(`${t('task_load_failed')}: ${error.message}`));
  const target = [...body.querySelectorAll('[data-task-comment]')].find((element) => element.dataset.taskComment === String(opts.commentId))
    || [...body.querySelectorAll('[data-task-event]')].find((element) => element.dataset.taskEvent === String(opts.eventId))?.closest('.tf-timeline-item');
  if (target) {
    target.classList.add('ps-task-target'); target.setAttribute('tabindex', '-1');
    requestAnimationFrame(() => { if (!signal.aborted) { target.scrollIntoView({ block: 'center' }); target.focus(); } });
  }
  signal.addEventListener('abort', () => {
    parentQuery += 1; galleryCleanup?.();
    if (Router.currentParams()?.projectId === pid && Router.currentParams()?.taskKey === tw.info?.task_key) updateProjectRoute();
  });
}

async function setTaskArchived(taskId, archived, pid = projectId()) {
  await ApiBinary.one('projectStudioTaskArchiveRequest', { projectId: pid, taskId, archived });
  if (state.project && projectId() === pid && state.tab === 'tasks' && (canArea('tasks') || canArea('board'))) await renderTasksTab();
  showUndoToast({ message: t(archived ? 'task_archived' : 'task_restored'), timeoutMs: 10000,
    onUndo: async () => {
      await ApiBinary.one('projectStudioTaskArchiveRequest', { projectId: pid, taskId, archived: !archived });
      if (projectId() === pid && state.tab === 'tasks' && (canArea('tasks') || canArea('board'))) await renderTasksTab();
    },
  });
}

function openTaskCommentEditor(comment, readers, onSaved) {
  const { body, foot, cleanup } = openWindow({ title: t('task_comment_edit_title'), icon: 'message', width: 620 });
  body.innerHTML = `<tf-textarea id="ps-comment-edit-body" label="${escapeAttr(t('task_comment_placeholder'))}" rows="5" autogrow></tf-textarea><tf-person-picker id="ps-comment-edit-mentions" multiple label="${escapeAttr(t('task_mentions_label'))}"></tf-person-picker><div class="ps-form-error" data-form-error hidden></div>`;
  body.querySelector('#ps-comment-edit-body').value = comment.body_md;
  const picker = body.querySelector('#ps-comment-edit-mentions');
  picker.items = readers.map((member) => ({ id: member.user_id, name: member.display_name }));
  picker.value = comment.mention_user_ids;
  foot.innerHTML = `<div></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" data-action="save">${escapeHtml(t('action_save'))}</tf-button></div>`;
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]');
    if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    const bodyMd = body.querySelector('#ps-comment-edit-body').value.trim();
    if (!bodyMd) return;
    button.setAttribute('disabled', '');
    try { await ApiBinary.one('projectStudioTaskCommentEditRequest', { projectId: projectId(), commentId: comment.comment_id, bodyMd, mentionUserIds: picker.value }); await onSaved(); cleanup(); }
    catch (error) { button.removeAttribute('disabled'); const errorEl = body.querySelector('[data-form-error]'); errorEl.hidden = false; errorEl.textContent = `${t('task_comment_failed')}: ${error.message}`; }
  });
}

function openTaskLinkWindow(sourceTaskId, onSaved) {
  const { body, foot, cleanup } = openWindow({ title: t('task_relation_add'), icon: 'branch', width: 620 });
  body.innerHTML = `<tf-combobox id="ps-task-link-target" label="${escapeAttr(t('task_relation_target'))}" placeholder="${escapeAttr(t('task_parent_search'))}" clearable></tf-combobox><tf-select id="ps-task-link-kind" label="${escapeAttr(t('task_relation_kind'))}" value="related">${TASK_LINK_KINDS.map((kind) => selectOpt(kind, 'related', t(`task_relation_${kind}`))).join('')}</tf-select><tf-input id="ps-task-link-lag" type="number" label="${escapeAttr(t('task_relation_lag'))}" value="0" min="-3650" max="3650" hint="${escapeAttr(t('task_relation_lag_hint'))}" disabled></tf-input><div class="ps-form-error" data-form-error hidden></div>`;
  foot.innerHTML = `<div></div><div class="ps-footer-right"><tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button><tf-button variant="primary" data-action="save">${escapeHtml(t('action_save'))}</tf-button></div>`;
  const target = body.querySelector('#ps-task-link-target');
  let targetTaskId = null;
  let queryId = 0;
  const showError = (message) => { const el = body.querySelector('[data-form-error]'); el.hidden = !message; el.textContent = message || ''; };
  const search = async (query = '') => {
    const request = ++queryId;
    const response = await ApiBinary.one('projectStudioTasksListRequest', { projectId: projectId(), scope: 'descendants', sourceProjectIds: [], taskType: '', status: '', assignedTo: '', search: query, severity: '', offset: 0, limit: 50 });
    if (!target.isConnected || queryId !== request) return;
    target.options = response.tasks.filter((task) => task.task_id !== sourceTaskId && !task.archived_at).map((task) => ({ value: task.task_id, label: `${task.task_key} · ${task.title}` }));
  };
  target.addEventListener('input', (event) => search(event.detail?.query || '').catch((error) => showError(error.message)));
  target.addEventListener('change', (event) => {
    targetTaskId = !event.detail?.free && target.options.some((option) => option.value === event.detail?.value) ? event.detail.value : null;
  });
  body.querySelector('#ps-task-link-kind').addEventListener('change', (event) => {
    const symmetric = ['related', 'duplicate'].includes(event.detail?.value);
    const lag = body.querySelector('#ps-task-link-lag');
    if (symmetric) { lag.setAttribute('disabled', ''); lag.value = '0'; } else lag.removeAttribute('disabled');
  });
  foot.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-action]');
    if (!button || button.hasAttribute('disabled')) return;
    if (button.dataset.action === 'cancel') { cleanup(); return; }
    if (!targetTaskId) { showError(t('err_task_relation_target')); return; }
    button.setAttribute('disabled', '');
    try { await ApiBinary.one('projectStudioTaskLinkSaveRequest', { projectId: projectId(), sourceTaskId, targetTaskId, kind: body.querySelector('#ps-task-link-kind').value, lagDays: Number(body.querySelector('#ps-task-link-lag').value) }); await onSaved(); cleanup(); }
    catch (error) { button.removeAttribute('disabled'); showError(`${t('task_relation_failed')}: ${error.message}`); }
  });
  search().catch((error) => showError(error.message));
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
      <tf-button variant="ghost" icon="bell" data-bell title="${escapeAttr(t('notif_title'))}"></tf-button>
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
        const text = taskNotificationText(body, t);
        const title = text ? text.title : body.title || t('notif_title');
        const message = text ? text.body : body.body;
        toast(`${title}${message ? ` — ${message}` : ''}`, 'info');
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
      listEl.innerHTML = `<tf-empty-state icon="bell" title="${escapeAttr(t('notif_empty'))}"></tf-empty-state>`;
      return;
    }
    listEl.innerHTML = `
      ${nw.items.map((n, i) => {
        const unread = !fv(n, 'read_at');
        const text = taskNotificationText(n, t);
        return `
          <div class="ps-notif-item ${unread ? 'is-unread' : ''}" data-notif="${i}" role="button" tabindex="0">
            <div class="ps-notif-ico">${sprite(NOTIF_KIND_ICON[n.kind] || 'info')}</div>
            <div class="ps-notif-main">
              <div class="ps-notif-title">${escapeHtml(text ? text.title : n.title || t(`nk_${n.kind}`))}</div>
              <div class="ps-notif-body">${escapeHtml(text ? text.body : n.body || '')}</div>
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
    const environmentId = link && link.environment_id;
    const pid = fv(n, 'project_id');
    if (!pid || !(runId || genId || taskId || environmentId)) return;
    cleanup();
    const sameProject = state.project && projectId() === pid;
    // An environment notification lands on the T12 list (admins can jump on to
    // the approval queue from there).
    if (environmentId) {
      if (sameProject) {
        if (state.tab !== 'tests') {
          state.tab = 'tests';
          renderTabsValue();
          await switchTab('tests');
        }
        f2().view = n.kind === 'environment_pending' && state.isAdmin ? 'env-approvals' : 'environments';
        f2().envs.loaded = false;
        await renderTestsView();
      } else {
        await openProject(pid, { tab: 'tests', sub: 'environments' });
      }
      return;
    }
    if (runId) {
      if (sameProject) await openRunDetailFromNotif(runId);
      else await openProject(pid, { tab: 'tests', runId });
    } else if (genId) {
      if (sameProject) await openGenDetailFromNotif(genId);
      else await openProject(pid, { tab: 'tests', sub: 'generations', genId });
    } else if (sameProject) {
      await openTaskWindowFromNotif(taskId, { eventId: link.event_id, commentId: link.comment_id });
    } else {
      await openProject(pid, { tab: 'tasks', taskId, eventId: link.event_id, commentId: link.comment_id });
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
  await openRunByType(runId);
}

async function openGenDetailFromNotif(genId) {
  if (state.tab !== 'tests') {
    state.tab = 'tests';
    renderTabsValue();
    await switchTab('tests');
  }
  await openGenDetail(genId);
}

async function openTaskWindowFromNotif(taskId, target) {
  await openProject(projectId(), { tab: 'tasks', taskId, ...target });
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

// =============================================================================
// T13 — run schedules (list, editor window, trigger history)
// =============================================================================

async function loadSchedules(force = false) {
  const s = f2();
  if (s.schedules.loaded && !force) return s.schedules.rows;
  const resp = await ApiBinary.one('projectStudioSchedulesListRequest', { projectId: projectId() });
  s.schedules.rows = Array.isArray(resp.schedules) ? resp.schedules : [];
  s.schedules.serverTimezone = fv(resp, 'server_timezone') || '';
  s.schedules.loaded = true;
  return s.schedules.rows;
}

// A schedule bound to an environment that is not approved never fires, even
// though its "enabled" switch is on — that state is the one worth shouting
// about, so it drives both the row accent and the result pill.
function scheduleBlocked(row) {
  const runType = fv(row, 'run_type');
  if (runType !== 'auto' && runType !== 'perf') return false;
  return fv(row, 'environment_status') !== 'approved';
}

function scheduleModeLabel(row) {
  const kind = fv(row, 'schedule_kind');
  const expr = fv(row, 'schedule_expr') || '';
  if (kind === 'cron') {
    const match = DAILY_CRON_RE.exec(expr.trim());
    const time = match ? `${String(match[2]).padStart(2, '0')}:${String(match[1]).padStart(2, '0')}` : expr;
    return t('sch_mode_cron_at', { time });
  }
  if (kind === 'once') return t('sch_mode_once_at', { at: formatTimestamp(expr) });
  return t('sch_mode_interval_every', { expr });
}

function scheduleLastChip(row) {
  if (scheduleBlocked(row)) return chipCell('warn', t('sch_last_blocked'));
  if (fv(row, 'auto_disabled')) return chipCell('err', t('sch_last_auto_disabled'));
  if (!row.enabled) return chipCell('info', t('sch_last_off'));
  const status = fv(row, 'last_status') || '';
  if (!status) return chipCell('info', t('sch_last_never'));
  // last_status is either a run status or a trigger outcome, depending on
  // whether the last attempt actually started a run.
  const runLabel = I18n.t(`project_studio.run_status_${status}`);
  if (runLabel !== `project_studio.run_status_${status}`) {
    return chipCell(SCHEDULE_LAST_CHIP[status] || 'info', runLabel);
  }
  const outcomeLabel = I18n.t(`project_studio.sch_outcome_${status}`);
  const known = outcomeLabel !== `project_studio.sch_outcome_${status}`;
  return chipCell(SCHEDULE_LAST_CHIP[status] || 'info', known ? outcomeLabel : status);
}

function scheduleNextCell(row) {
  if (!row.enabled) return `<span style="color:var(--text-3)">${escapeHtml(t('sch_next_disabled'))}</span>`;
  if (fv(row, 'auto_disabled')) return `<span style="color:var(--danger)">${escapeHtml(t('sch_next_auto_disabled'))}</span>`;
  if (scheduleBlocked(row)) return `<span style="color:var(--warning)">${escapeHtml(t('sch_next_blocked'))}</span>`;
  const next = fv(row, 'next_run_at');
  if (!next) return `<span style="color:var(--text-3)">—</span>`;
  return `<span>${escapeHtml(formatTimestamp(next))}</span>`;
}

function visibleSchedules() {
  const flt = f2().schedules;
  const query = flt.search.trim().toLowerCase();
  return f2().schedules.rows.filter((row) => {
    if (flt.kind && fv(row, 'schedule_kind') !== flt.kind) return false;
    if (flt.status === 'enabled' && !row.enabled) return false;
    if (flt.status === 'disabled' && row.enabled) return false;
    if (flt.status === 'blocked' && !scheduleBlocked(row) && !fv(row, 'auto_disabled')) return false;
    if (query) {
      const haystack = `${row.name} ${fv(row, 'suite_name') || ''} ${fv(row, 'environment_name') || ''}`.toLowerCase();
      if (!haystack.includes(query)) return false;
    }
    return true;
  });
}

// `reload` is false when only a client-side filter changed — the list is small
// and already in memory, so a refetch per keystroke would be pure noise.
async function renderSchedulesView(reload = true) {
  const host = byId('ps-tests-host');
  if (!host) return;
  const s = f2();
  try {
    await loadSchedules(reload);
  } catch (err) {
    host.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('sch_load_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'tests' || s.view !== 'schedules') return;

  const rows = visibleSchedules();
  const blocked = s.schedules.rows.filter((row) => scheduleBlocked(row) || fv(row, 'auto_disabled')).length;
  const active = s.schedules.rows.filter((row) => row.enabled && !fv(row, 'auto_disabled')).length;

  host.innerHTML = `
    <div class="ps-tests-toolbar ps-sch-toolbar">
      <tf-searchbox id="ps-sch-search" placeholder="${escapeAttr(t('sch_search_placeholder'))}" debounce="250" value="${escapeAttr(s.schedules.search)}"></tf-searchbox>
      <tf-select id="ps-sch-f-kind" value="${escapeAttr(s.schedules.kind)}">
        ${selectOpt('', s.schedules.kind, t('sch_filter_kind_all'))}
        ${SCHEDULE_KINDS.map((k) => selectOpt(k, s.schedules.kind, t(`sch_kind_${k}`))).join('')}
      </tf-select>
      <tf-select id="ps-sch-f-status" value="${escapeAttr(s.schedules.status)}">
        ${selectOpt('', s.schedules.status, t('sch_filter_status_all'))}
        ${selectOpt('enabled', s.schedules.status, t('sch_filter_status_enabled'))}
        ${selectOpt('disabled', s.schedules.status, t('sch_filter_status_disabled'))}
        ${selectOpt('blocked', s.schedules.status, t('sch_filter_status_blocked'))}
      </tf-select>
      <span class="ps-toolbar-spacer"></span>
      ${s.schedules.serverTimezone ? `<tf-chip status="info">${escapeHtml(t('sch_timezone_chip', { zone: s.schedules.serverTimezone }))}</tf-chip>` : ''}
      <tf-button variant="ghost" icon="refresh" id="ps-sch-refresh">${escapeHtml(t('refresh'))}</tf-button>
      ${canArea('tests', 'admin') ? `<tf-button variant="primary" icon="plus" id="ps-sch-new">${escapeHtml(t('sch_new'))}</tf-button>` : ''}
    </div>
    <div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('sch_intro'))}</span></div>
    <div id="ps-sch-table-host">
      ${rows.length ? '' : `<tf-empty-state icon="clock" title="${escapeAttr(t('sch_empty'))}"></tf-empty-state>`}
    </div>
    ${rows.length ? `<div class="ps-sch-foot">${escapeHtml(t('sch_foot', { shown: rows.length, total: s.schedules.rows.length, active, blocked }))}</div>` : ''}
  `;

  byId('ps-sch-search')?.addEventListener('search', (e) => {
    s.schedules.search = String(e.detail?.value ?? '');
    renderSchedulesView(false);
  });
  byId('ps-sch-f-kind')?.addEventListener('change', (e) => {
    s.schedules.kind = e.detail?.value ?? e.target.value ?? '';
    renderSchedulesView(false);
  });
  byId('ps-sch-f-status')?.addEventListener('change', (e) => {
    s.schedules.status = e.detail?.value ?? e.target.value ?? '';
    renderSchedulesView(false);
  });
  byId('ps-sch-refresh')?.addEventListener('click', () => renderSchedulesView());
  byId('ps-sch-new')?.addEventListener('click', () => openScheduleWindow(null));
  if (!rows.length) return;

  byId('ps-sch-table-host').innerHTML = `
    <tf-table id="ps-sch-table">
      <tf-column key="name" label="${escapeAttr(t('sch_col_name'))}" renderer="html"></tf-column>
      <tf-column key="mode" label="${escapeAttr(t('sch_col_mode'))}"></tf-column>
      <tf-column key="next" label="${escapeAttr(t('sch_col_next'))}" renderer="html"></tf-column>
      <tf-column key="last" label="${escapeAttr(t('sch_col_last'))}" renderer="chip"></tf-column>
    </tf-table>
  `;
  const table = byId('ps-sch-table');
  // tf-table renders its rows in shadow DOM, so the row state is carried by
  // inline styles on the html cells instead of a page-level row class.
  table.rows = rows.map((row) => {
    const isBlocked = scheduleBlocked(row);
    const dim = row.enabled ? '' : 'opacity:.62;';
    const accent = isBlocked ? 'border-left:3px solid var(--warning); padding-left:8px;' : '';
    const sub = [
      fv(row, 'suite_name') ? t('sch_sub_suite', { suite: fv(row, 'suite_name'), count: Number(fv(row, 'cases_count') ?? 0) })
        : t('sch_sub_cases', { count: Number(fv(row, 'cases_count') ?? 0) }),
      fv(row, 'environment_name') || t('sch_sub_no_env'),
      t(`run_type_${fv(row, 'run_type')}`),
    ].join(' · ');
    return {
      _id: fv(row, 'schedule_id'),
      _row: row,
      name: `
        <div style="display:flex; flex-direction:column; gap:3px; ${dim} ${accent}">
          <span style="font-weight:700">${escapeHtml(row.name)}</span>
          <span style="font-size:11px; color:var(--text-3)">${escapeHtml(sub)}</span>
          ${fv(row, 'auto_disabled') ? `<span style="font-size:11px; color:var(--danger)">${escapeHtml(t('sch_auto_disabled_badge', { count: Number(fv(row, 'consecutive_failures') ?? 0) }))}</span>` : ''}
        </div>
      `,
      mode: scheduleModeLabel(row),
      next: scheduleNextCell(row),
      last: scheduleLastChip(row),
    };
  });
  table.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    return buildScheduleRowActions(row._row, () => live()._row);
  };
  table.expandable = true;
  table.rowKey = '_id';
  table.expandRenderer = (row) => buildScheduleExpansion(row._row);
}

// tf-table renders this cell inside its shadow root, where page-level CSS does
// not reach — hence the inline layout and plain icon buttons instead of a
// kebab tf-menu, whose absolute popup would have no positioned ancestor here.
// `live` returns the schedule sitting at this table slot at call time; the cell
// element can outlive the row it was built from, so handlers must re-read it.
function buildScheduleRowActions(row, live = () => row) {
  const wrap = document.createElement('div');
  wrap.style.cssText = 'display:flex; align-items:center; gap:4px; justify-content:flex-end;';

  if (canArea('tests', 'admin')) {
    const toggle = document.createElement('tf-toggle');
    if (row.enabled) toggle.setAttribute('checked', '');
    if (!row.enabled && !canRunSchedule(row)) toggle.setAttribute('disabled', '');
    toggle.setAttribute('title', t(row.enabled ? 'sch_toggle_on' : 'sch_toggle_off'));
    toggle.addEventListener('change', async (e) => {
      e.stopPropagation();
      const enabled = !!(e.detail?.checked ?? toggle.hasAttribute('checked'));
      if (!canArea('tests', 'admin') || (enabled && !canRunSchedule(live()))) return;
      try {
        await ApiBinary.one('projectStudioScheduleSetEnabledRequest', {
          projectId: projectId(), scheduleId: fv(live(), 'schedule_id'), enabled,
        });
        toast(t(enabled ? 'sch_enabled_ok' : 'sch_disabled_ok'), 'success');
      } catch (err) {
        toast(`${t('sch_toggle_failed')}: ${err.message}`, 'error');
      }
      await renderSchedulesView();
    });
    wrap.appendChild(toggle);
  }

  const action = (icon, title, variant, handler) => {
    const btn = document.createElement('tf-button');
    btn.setAttribute('variant', variant);
    btn.setAttribute('size', 'sm');
    btn.setAttribute('icon', icon);
    btn.setAttribute('title', title);
    btn.addEventListener('click', (e) => { e.stopPropagation(); handler(); });
    wrap.appendChild(btn);
  };

  if (canRunSchedule(row)) action('play', t('sch_run_now'), 'ghost', () => runScheduleNow(live()));
  action('clock', t('sch_history'), 'ghost', () => openScheduleRunsWindow(live()));
  if (canArea('tests', 'admin')) {
    action('edit', t('action_edit'), 'ghost', () => openScheduleWindow(live()));
    action('trash', t('action_delete'), 'ghost', () => confirmDeleteSchedule(live()));
  }
  return wrap;
}

// The expansion carries the server-computed fire preview: the UI never derives
// those instants itself, otherwise the preview and the loop would disagree
// across a DST boundary.
function buildScheduleExpansion(row) {
  const wrap = document.createElement('div');
  wrap.className = 'ps-item-expansion';
  const preview = Array.isArray(fv(row, 'next_runs_preview')) ? fv(row, 'next_runs_preview') : [];
  const reason = fv(row, 'last_reason') || '';
  wrap.innerHTML = `
    ${scheduleBlocked(row) ? `<div class="ps-banner-warn">${sprite('alert')}<span>${escapeHtml(t('sch_blocked_banner'))}</span></div>` : ''}
    ${fv(row, 'auto_disabled') ? `<div class="ps-banner-warn">${sprite('alert')}<span>${escapeHtml(t('sch_breaker_banner'))}</span></div>` : ''}
    ${reason ? `<div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('sch_reason_prefix', { reason }))}</span></div>` : ''}
    <div class="ps-field-label">${escapeHtml(t('sch_preview_label'))}</div>
    <div class="ps-sch-preview">
      ${preview.length
        ? preview.map((at) => `<tf-chip status="info">${sprite('clock')} ${escapeHtml(formatTimestamp(at))}</tf-chip>`).join('')
        : `<span class="ps-field-hint">${escapeHtml(t('sch_preview_empty'))}</span>`}
    </div>
    <div class="ps-case-info-list">
      <div><span>${escapeHtml(t('sch_info_timezone'))}</span><b>${escapeHtml(fv(row, 'timezone') || f2().schedules.serverTimezone || '—')}</b></div>
      <div><span>${escapeHtml(t('sch_info_runner'))}</span><b>${escapeHtml(fv(row, 'runner_display_name') || t('run_runner_auto'))}</b></div>
      <div><span>${escapeHtml(t('sch_info_last_trigger'))}</span><b>${escapeHtml(fv(row, 'last_trigger_at') ? formatTimestamp(fv(row, 'last_trigger_at')) : '—')}</b></div>
      <div><span>${escapeHtml(t('sch_info_created_by'))}</span><b>${escapeHtml(fv(row, 'created_by_name') || '—')}</b></div>
    </div>
  `;
  return wrap;
}

async function runScheduleNow(row) {
  if (!canRunSchedule(row)) return;
  const scheduleId = fv(row, 'schedule_id');
  try {
    const resp = await ApiBinary.one('projectStudioScheduleRunNowRequest', { projectId: projectId(), scheduleId });
    const outcome = String(resp.outcome || '');
    const reason = String(resp.reason || '');
    const runNo = Number(fv(resp, 'run_no') ?? 0);
    if (outcome === 'started') {
      toast(t('sch_run_started', { no: runNo }), 'success');
      const runId = fv(resp, 'run_id');
      if (runId) await openRunByType(runId, fv(row, 'run_type'));
      return;
    }
    toast(t('sch_run_refused', { outcome: t(`sch_outcome_${outcome}`), reason: reason || '—' }), outcome === 'error' ? 'error' : 'info');
  } catch (err) {
    toast(`${t('sch_run_failed')}: ${err.message}`, 'error');
    return;
  }
  await renderSchedulesView();
}

function confirmDeleteSchedule(row) {
  openDeleteWindow({
    title: t('sch_delete_title'),
    targetName: row.name,
    targetSub: scheduleModeLabel(row),
    targetIcon: 'clock',
    warning: t('sch_delete_warning'),
    confirmLabel: t('delete_forever'),
    onConfirm: async () => {
      await ApiBinary.one('projectStudioScheduleDeleteRequest', {
        projectId: projectId(),
        scheduleId: fv(row, 'schedule_id'),
      });
      toast(t('sch_deleted_ok'), 'success');
      await renderSchedulesView();
    },
  });
}

async function openScheduleRunsWindow(row) {
  const { body } = openWindow({
    title: t('sch_history_title'),
    subtitle: row.name,
    icon: 'clock',
    width: 820,
  });
  body.innerHTML = `<div class="ps-loading">${escapeHtml(t('loading'))}</div>`;
  let runs = [];
  try {
    const resp = await ApiBinary.one('projectStudioScheduleRunsListRequest', {
      projectId: projectId(),
      scheduleId: fv(row, 'schedule_id'),
      limit: SCHEDULE_RUNS_LIMIT,
    });
    runs = Array.isArray(resp.runs) ? resp.runs : [];
  } catch (err) {
    body.innerHTML = `<div class="ps-form-error">${escapeHtml(err.message)}</div>`;
    return;
  }
  if (!runs.length) {
    body.innerHTML = `<tf-empty-state icon="clock" title="${escapeAttr(t('sch_history_empty'))}"></tf-empty-state>`;
    return;
  }
  body.innerHTML = `
    <tf-table id="ps-sch-runs-table">
      <tf-column key="fired" label="${escapeAttr(t('sch_hist_col_fired'))}"></tf-column>
      <tf-column key="scheduled" label="${escapeAttr(t('sch_hist_col_scheduled'))}"></tf-column>
      <tf-column key="outcome" label="${escapeAttr(t('sch_hist_col_outcome'))}" renderer="chip"></tf-column>
      <tf-column key="reason" label="${escapeAttr(t('sch_hist_col_reason'))}"></tf-column>
      <tf-column key="run" label="${escapeAttr(t('sch_hist_col_run'))}"></tf-column>
      <tf-column key="actor" label="${escapeAttr(t('sch_hist_col_actor'))}"></tf-column>
    </tf-table>
  `;
  const table = body.querySelector('#ps-sch-runs-table');
  table.rows = runs.map((entry) => ({
    _runId: fv(entry, 'run_id'),
    fired: formatTimestamp(fv(entry, 'fired_at')),
    scheduled: formatTimestamp(fv(entry, 'scheduled_for')),
    outcome: chipCell(SCHEDULE_OUTCOME_CHIP[entry.outcome] || 'info', t(`sch_outcome_${entry.outcome}`)),
    reason: entry.reason || '—',
    run: Number(fv(entry, 'run_no') ?? 0) > 0 ? `#${fv(entry, 'run_no')}` : '—',
    actor: fv(entry, 'actor_name') || t('sch_actor_loop'),
  }));
  table.addEventListener('row-click', async (e) => {
    const runId = e.detail?.row?._runId;
    if (!runId) return;
    closeAllWindows();
    await openRunByType(runId, fv(row, 'run_type'));
  });
}

// The IANA catalogue comes from the browser; the server zone and the local zone
// are always offered so the picker is never empty on older engines.
function timezoneOptions(serverZone) {
  let zones = [];
  try {
    if (typeof Intl.supportedValuesOf === 'function') zones = Intl.supportedValuesOf('timeZone');
  } catch {
    zones = [];
  }
  let local = '';
  try {
    local = Intl.DateTimeFormat().resolvedOptions().timeZone || '';
  } catch {
    local = '';
  }
  return [...new Set([serverZone, local, 'UTC', ...zones].filter(Boolean))];
}

// 'cron' is stored as a daily "minute hour * * *" expression but typed as HH:MM
// (the T13 mockup) — these two helpers are the only place that conversion lives.
function cronToTime(expr) {
  const match = DAILY_CRON_RE.exec(String(expr || '').trim());
  if (!match) return '';
  return `${String(match[2]).padStart(2, '0')}:${String(match[1]).padStart(2, '0')}`;
}

function timeToCron(value) {
  const match = /^([01]?\d|2[0-3]):([0-5]\d)$/.exec(String(value || '').trim());
  if (!match) return '';
  return `${Number(match[2])} ${Number(match[1])} * * *`;
}

// datetime-local wants "YYYY-MM-DDTHH:MM" in local time; the wire carries an
// RFC3339 instant.
function isoToLocalInput(iso) {
  if (!iso) return '';
  const date = new Date(String(iso));
  if (Number.isNaN(date.getTime())) return '';
  const pad = (n) => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function intervalMinutes(expr) {
  const match = /^(\d+)([mhd])$/.exec(String(expr || '').trim());
  if (!match) return null;
  const value = Number(match[1]);
  if (!Number.isFinite(value) || value <= 0) return null;
  return value * ({ m: 1, h: 60, d: 1440 })[match[2]];
}

// T13 window — create / edit. The whole definition travels in one ScheduleSave,
// so every field on the form is written, including the cleared ones.
function openScheduleWindow(schedule) {
  if (!canArea('tests', 'admin')) return;
  const editing = !!schedule;
  const { body, foot, cleanup } = openWindow({
    title: t(editing ? 'sch_win_edit_title' : 'sch_win_new_title'),
    subtitle: editing ? schedule.name : t('sch_win_sub'),
    icon: 'clock',
    width: 760,
  });

  const s = f2();
  const serverZone = s.schedules.serverTimezone || '';
  const kind = editing ? (fv(schedule, 'schedule_kind') || 'interval') : 'interval';
  const sw = {
    runType: editing ? (fv(schedule, 'run_type') || 'auto') : canTryRun() ? 'auto' : 'manual',
    source: editing && !fv(schedule, 'suite_id') ? 'cases' : 'suite',
    suiteId: editing ? (fv(schedule, 'suite_id') || '') : '',
    caseIds: new Set(editing && Array.isArray(fv(schedule, 'case_ids')) ? fv(schedule, 'case_ids') : []),
    environmentId: editing ? (fv(schedule, 'environment_id') || '') : '',
    runnerServiceId: editing ? (fv(schedule, 'runner_service_id') || '') : '',
    mode: editing ? (fv(schedule, 'assignment_mode') || 'pool') : 'pool',
    assignees: new Set(editing && Array.isArray(fv(schedule, 'assignees')) ? fv(schedule, 'assignees') : []),
    kind,
    timezone: editing ? (fv(schedule, 'timezone') || serverZone) : serverZone,
    pool: [],
    busy: false,
  };
  const perf = (() => {
    try {
      const parsed = JSON.parse(fv(schedule, 'perf_profile_json') || '{}');
      return { ...PERF_DEFAULT_PROFILE, ...(parsed && typeof parsed === 'object' ? parsed : {}) };
    } catch {
      return { ...PERF_DEFAULT_PROFILE };
    }
  })();
  const exprValue = {
    interval: kind === 'interval' ? (fv(schedule, 'schedule_expr') || '') : '',
    cron: kind === 'cron' ? cronToTime(fv(schedule, 'schedule_expr')) : '',
    once: kind === 'once' ? isoToLocalInput(fv(schedule, 'schedule_expr')) : '',
  };
  const preview = editing && Array.isArray(fv(schedule, 'next_runs_preview')) ? fv(schedule, 'next_runs_preview') : [];

  body.innerHTML = `
    <tf-input id="ps-sch-name" label="${escapeAttr(t('sch_name_label'))}" placeholder="${escapeAttr(t('sch_name_placeholder'))}"
      value="${escapeAttr(editing ? schedule.name : '')}" hint="${escapeAttr(t('sch_name_hint'))}"></tf-input>

    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('run_type_label'))}</span>
      <tf-segmented id="ps-sch-runtype" value="${escapeAttr(sw.runType)}"></tf-segmented>
      <div class="ps-field-hint" data-sch-runtype-hint></div>
    </div>

    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('sch_source_label'))}</span>
      <tf-segmented id="ps-sch-source" value="${escapeAttr(sw.source)}">
        <option value="suite">${escapeHtml(t('run_source_suite'))}</option>
        <option value="cases">${escapeHtml(t('run_source_cases'))}</option>
      </tf-segmented>
      <div class="ps-field-hint">${escapeHtml(t('sch_source_hint'))}</div>
    </div>
    <div data-sch-source="suite" hidden>
      <tf-select id="ps-sch-suite" label="${escapeAttr(t('run_suite_label'))}" value="${escapeAttr(sw.suiteId)}"></tf-select>
    </div>
    <div data-sch-source="cases" hidden>
      <div class="ps-field">
        <span class="ps-field-label">${escapeHtml(t('run_cases_label'))}</span>
        <div class="ps-pane-list ps-run-case-pool" data-sch-case-pool></div>
      </div>
    </div>

    <div data-sch-auto hidden>
      <div class="ps-field">
        <tf-select id="ps-sch-env" label="${escapeAttr(t('run_env_select_label'))}" value="${escapeAttr(sw.environmentId)}"></tf-select>
        <div class="ps-field-hint">${escapeHtml(t('sch_env_hint'))}</div>
      </div>
      <div class="ps-banner-warn" data-sch-env-warn hidden>${sprite('alert')}<span>${escapeHtml(t('sch_env_warn'))}</span></div>
      <div class="ps-field">
        <tf-select id="ps-sch-runner" label="${escapeAttr(t('run_runner_label'))}" value="${escapeAttr(sw.runnerServiceId)}"></tf-select>
        <div class="ps-field-hint">${escapeHtml(t('run_runner_hint'))}</div>
      </div>
      <div class="ps-field" data-sch-perf hidden>
        <span class="ps-field-label">${escapeHtml(t('run_perf_label'))}</span>
        <div class="ps-perf-form">
          <tf-input id="ps-sch-perf-users" type="number" min="${PERF_LIMITS.users[0]}" max="${PERF_LIMITS.users[1]}"
            label="${escapeAttr(t('run_perf_users_label'))}" value="${Number(perf.users)}"></tf-input>
          <tf-input id="ps-sch-perf-spawn" type="number" min="${PERF_LIMITS.spawnRate[0]}" max="${PERF_LIMITS.spawnRate[1]}"
            label="${escapeAttr(t('run_perf_spawn_label'))}" value="${Number(perf.spawn_rate)}"></tf-input>
          <tf-input id="ps-sch-perf-duration" type="number" min="${PERF_LIMITS.duration[0]}" max="${PERF_LIMITS.duration[1]}"
            label="${escapeAttr(t('run_perf_duration_label'))}" value="${Number(perf.duration_secs)}"></tf-input>
        </div>
        <div class="ps-field-hint">${escapeHtml(t('run_perf_hint'))}</div>
      </div>
    </div>

    <div data-sch-manual hidden>
      <div class="ps-field">
        <span class="ps-field-label">${escapeHtml(t('run_assign_label'))}</span>
        <tf-segmented id="ps-sch-mode" value="${escapeAttr(sw.mode === 'single' ? 'single' : 'pool')}">
          <option value="single">${escapeHtml(t('assign_single'))}</option>
          <option value="pool">${escapeHtml(t('assign_pool'))}</option>
        </tf-segmented>
        <div class="ps-field-hint">${escapeHtml(t('sch_assign_hint'))}</div>
      </div>
      <div class="ps-field">
        <span class="ps-field-label">${escapeHtml(t('sch_assignees_label'))}</span>
        <div class="ps-pane-list" data-sch-assignees></div>
      </div>
    </div>

    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('sch_kind_label'))}</span>
      <tf-segmented id="ps-sch-kind" value="${escapeAttr(sw.kind)}">
        ${SCHEDULE_KINDS.map((k) => `<option value="${k}">${escapeHtml(t(`sch_kind_${k}`))}</option>`).join('')}
      </tf-segmented>
      <div class="ps-field-hint">${escapeHtml(t('sch_kind_hint'))}</div>
    </div>
    <div data-sch-expr="interval" hidden>
      <tf-input id="ps-sch-expr-interval" label="${escapeAttr(t('sch_expr_interval_label'))}" placeholder="6h"
        value="${escapeAttr(exprValue.interval)}"></tf-input>
    </div>
    <div data-sch-expr="cron" hidden>
      <tf-input id="ps-sch-expr-cron" type="time" label="${escapeAttr(t('sch_expr_cron_label'))}"
        value="${escapeAttr(exprValue.cron)}"></tf-input>
    </div>
    <div data-sch-expr="once" hidden>
      <tf-input id="ps-sch-expr-once" type="datetime-local" label="${escapeAttr(t('sch_expr_once_label'))}"
        value="${escapeAttr(exprValue.once)}"></tf-input>
    </div>
    <div class="ps-sch-validation" data-sch-validation></div>

    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('sch_timezone_label'))}</span>
      <tf-combobox id="ps-sch-tz" placeholder="${escapeAttr(t('sch_timezone_placeholder'))}" value="${escapeAttr(sw.timezone)}"></tf-combobox>
      <div class="ps-field-hint">${escapeHtml(t('sch_timezone_hint', { zone: serverZone || 'UTC' }))}</div>
    </div>

    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('sch_preview_label'))}</span>
      <div class="ps-sch-preview" data-sch-preview>
        ${preview.length
          ? preview.map((at) => `<tf-chip status="info">${sprite('clock')} ${escapeHtml(formatTimestamp(at))}</tf-chip>`).join('')
          : `<span class="ps-field-hint">${escapeHtml(t('sch_preview_hint'))}</span>`}
      </div>
    </div>

    <div class="ps-toggle-inline">
      <tf-toggle id="ps-sch-enabled" ${!editing || schedule.enabled ? 'checked' : ''}></tf-toggle>
      <span>${escapeHtml(t('sch_enabled_label'))}</span>
    </div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  body.querySelector('#ps-sch-runtype').setOptions(RUN_TYPES.map((value) => ({ value, label: t(`run_type_${value}`), disabled: value !== 'manual' && !canTryRun() })), sw.runType);
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="primary" icon="check" data-action="save">${escapeHtml(t(editing ? 'action_save' : 'sch_create'))}</tf-button>
    </div>
  `;

  const tzBox = body.querySelector('#ps-sch-tz');
  if (tzBox) {
    tzBox.options = timezoneOptions(serverZone).map((zone) => ({ value: zone, label: zone }));
    tzBox.addEventListener('change', (e) => { sw.timezone = e.detail?.value || ''; });
  }

  const showError = (msg) => {
    const el = body.querySelector('[data-form-error]');
    if (el) { el.hidden = !msg; el.textContent = msg || ''; }
  };
  const isAutomated = () => sw.runType !== 'manual';

  // Live syntax check only: the fire instants themselves stay server-side.
  const validateExpr = () => {
    const box = body.querySelector('[data-sch-validation]');
    let ok = true;
    let message = '';
    if (sw.kind === 'interval') {
      const raw = String(body.querySelector('#ps-sch-expr-interval')?.value ?? '').trim();
      const minutes = INTERVAL_RE.test(raw) ? intervalMinutes(raw) : null;
      if (minutes === null) { ok = false; message = t('sch_err_interval_format'); }
      else if (minutes < 5) { ok = false; message = t('sch_err_interval_min'); }
      else if (minutes > 365 * 1440) { ok = false; message = t('sch_err_interval_max'); }
      else message = t('sch_ok_interval', { expr: raw });
    } else if (sw.kind === 'cron') {
      const raw = String(body.querySelector('#ps-sch-expr-cron')?.value ?? '').trim();
      if (!timeToCron(raw)) { ok = false; message = t('sch_err_cron_format'); }
      else message = t('sch_ok_cron', { time: raw, zone: sw.timezone || serverZone || 'UTC' });
    } else {
      const raw = String(body.querySelector('#ps-sch-expr-once')?.value ?? '').trim();
      const date = raw ? new Date(raw) : null;
      if (!date || Number.isNaN(date.getTime())) { ok = false; message = t('sch_err_once_format'); }
      else if (date.getTime() <= Date.now()) { ok = false; message = t('sch_err_once_past'); }
      else message = t('sch_ok_once', { at: date.toLocaleString(I18n.getLanguage()) });
    }
    if (box) {
      box.className = `ps-sch-validation ${ok ? 'is-ok' : 'is-err'}`;
      box.textContent = message;
    }
    return ok;
  };

  const syncKind = () => {
    body.querySelectorAll('[data-sch-expr]').forEach((el) => { el.hidden = el.dataset.schExpr !== sw.kind; });
    validateExpr();
  };

  const syncRunType = () => {
    const automated = isAutomated();
    const autoBox = body.querySelector('[data-sch-auto]');
    const manualBox = body.querySelector('[data-sch-manual]');
    if (autoBox) autoBox.hidden = !automated || !canTryRun();
    if (manualBox) manualBox.hidden = automated;
    const perfBox = body.querySelector('[data-sch-perf]');
    if (perfBox) perfBox.hidden = sw.runType !== 'perf';
    const hint = body.querySelector('[data-sch-runtype-hint]');
    if (hint) hint.textContent = t(`run_type_${sw.runType}_hint`);
    const save = foot.querySelector('[data-action="save"]');
    if (save) save.toggleAttribute('disabled', !canArea('tests', 'admin') || (automated && !canTryRun()));
    syncEnvWarning();
  };

  const syncEnvWarning = () => {
    const warn = body.querySelector('[data-sch-env-warn]');
    if (!warn) return;
    const env = (f2().envs.rows || []).find((e) => fv(e, 'environment_id') === sw.environmentId);
    warn.hidden = !isAutomated() || !sw.environmentId || fv(env, 'approval_status') === 'approved';
  };

  const renderCasePool = () => {
    const hostEl = body.querySelector('[data-sch-case-pool]');
    if (!hostEl) return;
    if (!sw.pool.length) {
      hostEl.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('run_cases_empty'))}</div>`;
      return;
    }
    hostEl.innerHTML = sw.pool.map((c) => {
      const caseId = fv(c, 'case_id');
      return `
        <div class="ps-pane-row">
          <tf-checkbox data-sch-case="${escapeAttr(caseId)}" ${sw.caseIds.has(caseId) ? 'checked' : ''}></tf-checkbox>
          <div class="ps-pane-row-main">
            <div class="ps-pane-row-title">${escapeHtml(c.title)}</div>
            <tf-chip status="${PRIORITY_CHIP[c.priority] || 'info'}">${escapeHtml(t(`prio_${c.priority}`))}</tf-chip>
          </div>
        </div>
      `;
    }).join('');
    hostEl.querySelectorAll('[data-sch-case]').forEach((cb) => {
      cb.addEventListener('change', () => {
        const id = cb.dataset.schCase;
        if (cb.checked) sw.caseIds.add(id);
        else sw.caseIds.delete(id);
      });
    });
  };

  const renderAssignees = () => {
    const hostEl = body.querySelector('[data-sch-assignees]');
    if (!hostEl) return;
    const testers = testerMembers();
    if (!testers.length) {
      hostEl.innerHTML = `<div class="ps-field-hint">${escapeHtml(t('sch_assignees_empty'))}</div>`;
      return;
    }
    hostEl.innerHTML = testers.map((m) => {
      const userId = fv(m, 'user_id');
      return `
        <div class="ps-pane-row">
          <tf-checkbox data-sch-assignee="${escapeAttr(userId)}" ${sw.assignees.has(userId) ? 'checked' : ''}></tf-checkbox>
          <div class="ps-pane-row-main">
            <div class="ps-pane-row-title">${escapeHtml(fv(m, 'display_name') || '')}</div>
            ${functionChips(m.functions)}
          </div>
        </div>
      `;
    }).join('');
    hostEl.querySelectorAll('[data-sch-assignee]').forEach((cb) => {
      cb.addEventListener('change', () => {
        const id = cb.dataset.schAssignee;
        if (cb.checked) {
          // "single" is exactly one tester, so a new pick replaces the old one.
          if (sw.mode === 'single') { sw.assignees.clear(); renderAssignees(); }
          sw.assignees.add(id);
        } else {
          sw.assignees.delete(id);
        }
      });
    });
  };

  const syncSource = () => {
    body.querySelectorAll('[data-sch-source]').forEach((el) => { el.hidden = el.dataset.schSource !== sw.source; });
  };

  body.querySelector('#ps-sch-runtype')?.addEventListener('change', (e) => {
    if (e.detail?.value !== 'manual' && !canTryRun()) {
      e.target.setAttribute('value', sw.runType);
      return;
    }
    sw.runType = e.detail?.value ?? sw.runType;
    syncRunType();
  });
  body.querySelector('#ps-sch-source')?.addEventListener('change', (e) => {
    sw.source = e.detail?.value ?? sw.source;
    syncSource();
  });
  body.querySelector('#ps-sch-kind')?.addEventListener('change', (e) => {
    sw.kind = e.detail?.value ?? sw.kind;
    syncKind();
  });
  body.querySelector('#ps-sch-mode')?.addEventListener('change', (e) => {
    sw.mode = e.detail?.value ?? sw.mode;
    if (sw.mode === 'single' && sw.assignees.size > 1) {
      const first = [...sw.assignees][0];
      sw.assignees = new Set([first]);
    }
    renderAssignees();
  });
  body.querySelector('#ps-sch-env')?.addEventListener('change', (e) => {
    sw.environmentId = e.detail?.value ?? '';
    syncEnvWarning();
  });
  body.querySelector('#ps-sch-runner')?.addEventListener('change', (e) => {
    sw.runnerServiceId = e.detail?.value ?? '';
  });
  body.querySelector('#ps-sch-suite')?.addEventListener('change', (e) => { sw.suiteId = e.detail?.value ?? ''; });
  ['#ps-sch-expr-interval', '#ps-sch-expr-cron', '#ps-sch-expr-once'].forEach((sel) => {
    body.querySelector(sel)?.addEventListener('input', () => validateExpr());
    body.querySelector(sel)?.addEventListener('change', () => validateExpr());
  });

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || sw.busy) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    if (!canArea('tests', 'admin') || (isAutomated() && !canTryRun())) return;

    const name = String(body.querySelector('#ps-sch-name')?.value ?? '').trim();
    if (name.length < 3) { showError(t('sch_err_name')); return; }
    if (sw.source === 'suite' && !sw.suiteId) { showError(t('err_run_suite')); return; }
    if (sw.source === 'cases' && !sw.caseIds.size) { showError(t('err_run_cases')); return; }
    if (!validateExpr()) { showError(t('sch_err_expression')); return; }
    if (isAutomated() && !sw.environmentId) { showError(t('err_run_env')); return; }
    if (!isAutomated() && sw.mode === 'single' && sw.assignees.size !== 1) { showError(t('sch_err_single_assignee')); return; }

    let scheduleExpr = '';
    if (sw.kind === 'interval') scheduleExpr = String(body.querySelector('#ps-sch-expr-interval')?.value ?? '').trim();
    else if (sw.kind === 'cron') scheduleExpr = timeToCron(body.querySelector('#ps-sch-expr-cron')?.value);
    else scheduleExpr = new Date(String(body.querySelector('#ps-sch-expr-once')?.value ?? '')).toISOString();

    let perfProfileJson = '';
    if (sw.runType === 'perf') {
      const users = Number(body.querySelector('#ps-sch-perf-users')?.value ?? 0);
      const spawnRate = Number(body.querySelector('#ps-sch-perf-spawn')?.value ?? 0);
      const duration = Number(body.querySelector('#ps-sch-perf-duration')?.value ?? 0);
      const inRange = (value, [min, max]) => Number.isFinite(value) && value >= min && value <= max;
      if (!inRange(users, PERF_LIMITS.users) || !inRange(spawnRate, PERF_LIMITS.spawnRate)
        || !inRange(duration, PERF_LIMITS.duration)) {
        showError(t('err_run_perf'));
        return;
      }
      perfProfileJson = JSON.stringify({ users, spawn_rate: spawnRate, duration_secs: duration });
    }

    showError(null);
    sw.busy = true;
    btn.setAttribute('disabled', '');
    try {
      const resp = await ApiBinary.one('projectStudioScheduleSaveRequest', {
        projectId: projectId(),
        scheduleId: editing ? fv(schedule, 'schedule_id') : null,
        name,
        runType: sw.runType,
        suiteId: sw.source === 'suite' ? sw.suiteId : '',
        caseIds: sw.source === 'cases' ? [...sw.caseIds] : [],
        environmentId: isAutomated() ? sw.environmentId : '',
        runnerServiceId: isAutomated() ? sw.runnerServiceId : '',
        perfProfileJson,
        assignmentMode: isAutomated() ? '' : sw.mode,
        assignees: isAutomated() ? [] : [...sw.assignees],
        scheduleKind: sw.kind,
        scheduleExpr,
        timezone: sw.timezone || serverZone,
        enabled: !!body.querySelector('#ps-sch-enabled')?.checked,
      });
      const nextAt = fv(resp, 'next_run_at');
      toast(nextAt ? t('sch_saved_next', { at: formatTimestamp(nextAt) }) : t('sch_saved'), 'success');
      cleanup();
      f2().schedules.loaded = false;
      if (state.tab === 'tests' && f2().view === 'schedules') await renderTestsView();
    } catch (err) {
      sw.busy = false;
      btn.removeAttribute('disabled');
      showError(`${t('sch_save_failed')}: ${err.message}`);
    }
  });

  syncSource();
  syncRunType();
  syncKind();

  // Suites / approved cases / environments / runners / members load lazily.
  (async () => {
    await ensureF2Members();
    renderAssignees();
    let suites = s.suites;
    if (!suites.length) {
      try {
        const resp = await ApiBinary.one('projectStudioSuitesListRequest', { projectId: projectId() });
        suites = Array.isArray(resp.suites) ? resp.suites : [];
        s.suites = suites;
      } catch {
        suites = [];
      }
    }
    body.querySelector('#ps-sch-suite')?.setOptions([
      { value: '', label: t('assign_choose') },
      ...suites.map((su) => ({ value: fv(su, 'suite_id'), label: `${su.name} (${Number(fv(su, 'case_count') ?? 0)})` })),
    ], sw.suiteId);
    try {
      const resp = await ApiBinary.one('projectStudioCasesListRequest', {
        projectId: projectId(), status: 'approved', offset: 0, limit: 200,
      });
      sw.pool = Array.isArray(resp.cases) ? resp.cases : [];
    } catch {
      sw.pool = [];
    }
    renderCasePool();
    if (canArea('environments')) {
      try {
        await loadEnvironments();
      } catch {
        /* the select stays empty and the save guard blocks an automated schedule */
      }
    }
    const approved = approvedEnvironments();
    body.querySelector('#ps-sch-env')?.setOptions([
      { value: '', label: approved.length ? t('assign_choose') : t('run_env_none') },
      ...approved.map((env) => ({ value: fv(env, 'environment_id'), label: `${env.name} — ${fv(env, 'base_url')}` })),
    ], sw.environmentId);
    syncEnvWarning();
    await loadRunners();
    body.querySelector('#ps-sch-runner')?.setOptions([
      { value: '', label: t('run_runner_auto') },
      ...(f2().runners || []).map((r) => ({
        value: fv(r, 'service_id'),
        label: `${fv(r, 'display_name') || fv(r, 'engine_id')} — ${runnerToolchainLabel(r)}`,
      })),
    ], sw.runnerServiceId);
  })();
}

// =============================================================================
// X02 — ML Studio connections
// =============================================================================

function connections() {
  if (!state.connections) state.connections = { links: [], canManage: false, loaded: false };
  return state.connections;
}

async function loadMlLinks() {
  const conn = connections();
  const resp = await ApiBinary.one('projectStudioMlLinksListRequest', { projectId: projectId() });
  conn.links = Array.isArray(resp.links) ? resp.links : [];
  conn.canManage = !!fv(resp, 'can_manage') && canArea('settings', 'write') && canArea('repos', 'write');
  conn.loaded = true;
  return conn.links;
}

// The wire deep link is a dashboard route ("ml-studio?projectId=..."); anything
// unparseable still lands on the ML Studio project route.
function openMlStudio(link) {
  const mlProjectId = fv(link, 'ml_project_id');
  let view = 'ml-studio';
  const params = { projectId: mlProjectId };
  const raw = String(fv(link.summary || {}, 'deep_link') || '').trim().replace(/^#?\/?/, '');
  if (raw) {
    const [path, query] = raw.split('?');
    if (/^[a-z0-9-]+$/.test(path)) view = path;
    if (query) {
      for (const [key, value] of new URLSearchParams(query)) params[key] = value;
    }
  }
  Router.navigate(view, params);
}

// last_training_metrics_json is model-specific, so the card renders whatever
// numeric metrics the training run reported instead of a fixed set.
function mlMetricChips(summary) {
  let metrics = null;
  try {
    metrics = JSON.parse(fv(summary, 'last_training_metrics_json') || '{}');
  } catch {
    metrics = null;
  }
  if (!metrics || typeof metrics !== 'object') return [];
  return Object.entries(metrics)
    .filter(([, value]) => typeof value === 'number' && Number.isFinite(value))
    .slice(0, 4)
    .map(([key, value]) => ({ key, value: Math.round(value * 10000) / 10000 }));
}

function mlCardHtml(link) {
  const summary = link.summary || null;
  const linkId = fv(link, 'link_id');
  const canOpen = !!fv(link, 'can_open');
  const training = summary ? !!fv(summary, 'training_in_progress') : false;
  const lastStatus = summary ? String(fv(summary, 'last_training_status') || '') : '';
  const statusChip = !summary
    ? `<tf-chip status="err" dot>${escapeHtml(t('ml_summary_unavailable'))}</tf-chip>`
    : (training
      ? `<tf-chip status="accent" dot>${escapeHtml(t('ml_training_running'))}</tf-chip>`
      : `<tf-chip status="${lastStatus === 'completed' ? 'ok' : (lastStatus ? 'warn' : 'info')}" dot>${escapeHtml(lastStatus ? t('ml_training_last', { status: lastStatus }) : t('ml_training_none'))}</tf-chip>`);
  const models = summary && Array.isArray(fv(summary, 'models')) ? fv(summary, 'models') : [];
  const metrics = summary ? mlMetricChips(summary) : [];
  const syncChip = fv(link, 'sync_permissions')
    ? `<tf-chip status="accent">${sprite('refresh')} ${escapeHtml(t('ml_sync_on'))}</tf-chip>`
    : `<tf-chip status="info">${escapeHtml(t('ml_sync_off'))}</tf-chip>`;
  const lastSyncResult = fv(link, 'last_sync_result') || '';

  const menuItems = [
    `<tf-menu-item action="open" icon="external-link" ${canOpen ? '' : 'disabled'}>${escapeHtml(t('ml_open'))}</tf-menu-item>`,
  ];
  if (connections().canManage) {
    menuItems.push(`<tf-menu-item action="settings" icon="settings">${escapeHtml(t('ml_link_settings'))}</tf-menu-item>`);
    menuItems.push(`<tf-menu-item action="sync" icon="refresh">${escapeHtml(t('ml_sync_now'))}</tf-menu-item>`);
    menuItems.push('<tf-menu-divider></tf-menu-divider>');
    menuItems.push(`<tf-menu-item action="detach" icon="trash" danger>${escapeHtml(t('ml_detach'))}</tf-menu-item>`);
  }

  return `
    <div class="ps-ml-card" data-ml-link="${escapeAttr(linkId)}">
      <div class="ps-ml-top">
        <div class="ps-card-ico">${sprite('brain')}</div>
        <div class="ps-ml-heading">
          <div class="ps-ml-name">${escapeHtml(fv(link, 'label') || (summary ? summary.name : fv(link, 'ml_project_id')))}${statusChip}</div>
          <div class="ps-ml-desc">${escapeHtml(summary
            ? t('ml_card_desc', { type: fv(summary, 'project_type_label') || fv(summary, 'project_type') || '—' })
            : t('ml_card_desc_unavailable'))}</div>
        </div>
        <div class="ps-card-menu-wrap">
          <tf-button variant="ghost" size="sm" icon="chevron-down" data-ml-more title="${escapeAttr(t('action_more'))}"></tf-button>
          <tf-menu placement="bottom-end" data-ml-menu>${menuItems.join('')}</tf-menu>
        </div>
      </div>
      ${models.length ? `<div class="ps-ml-models">${models.map((m) => `<tf-chip status="info">${escapeHtml(m)}</tf-chip>`).join('')}</div>` : ''}
      ${training ? `
        <div class="ps-ml-progress">
          <div class="ps-ml-progress-track"><span></span></div>
          <div class="ps-ml-progress-text"><tf-spinner size="sm"></tf-spinner>${escapeHtml(t('ml_training_progress', { started: formatTimestamp(fv(summary, 'last_training_started_at')) }))}</div>
        </div>
      ` : ''}
      ${summary ? `
        <div class="ps-ml-stats">
          ${metrics.map((m) => `<span class="ps-ml-stat">${escapeHtml(m.key)} <b>${escapeHtml(String(m.value))}</b></span>`).join('')}
          <span class="ps-ml-stat">${escapeHtml(t('ml_stat_datasets'))} <b>${Number(fv(summary, 'dataset_count') ?? 0)}</b></span>
          <span class="ps-ml-stat">${escapeHtml(t('ml_stat_models'))} <b>${Number(fv(summary, 'model_count') ?? 0)}</b></span>
          ${fv(summary, 'last_training_finished_at') ? `<span class="ps-ml-stat">${escapeHtml(t('ml_stat_trained'))} <b>${escapeHtml(formatTimestamp(fv(summary, 'last_training_finished_at')))}</b></span>` : ''}
        </div>
      ` : ''}
      <div class="ps-ml-foot">
        <tf-button variant="primary" size="sm" icon="external-link" data-ml-open ${canOpen ? '' : 'disabled'}
          title="${escapeAttr(canOpen ? t('ml_open') : t('ml_open_denied'))}">${escapeHtml(t('ml_open'))}</tf-button>
        ${syncChip}
        ${lastSyncResult ? `<tf-chip status="${lastSyncResult === 'ok' ? 'ok' : 'warn'}">${escapeHtml(t('ml_last_sync', { result: lastSyncResult }))}</tf-chip>` : ''}
      </div>
    </div>
  `;
}

async function renderConnections() {
  const panel = byId('ps-tab-panel');
  if (!panel) return;
  const conn = connections();
  try {
    await loadMlLinks();
  } catch (err) {
    panel.innerHTML = `<div class="ps-form-error">${escapeHtml(`${t('ml_load_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (state.tab !== 'connections') return;

  panel.innerHTML = `
    <tf-section-card title="${escapeAttr(t('ml_section_title'))}" icon="brain">
      <span slot="actions">
        ${conn.canManage ? `
          <tf-button variant="ghost" size="sm" icon="share" id="ps-ml-attach">${escapeHtml(t('ml_attach_existing'))}</tf-button>
          <tf-button variant="primary" size="sm" icon="plus" id="ps-ml-create">${escapeHtml(t('ml_create'))}</tf-button>
        ` : ''}
      </span>
      <div class="ps-section-sub">${escapeHtml(t('ml_section_sub'))}</div>
      <div id="ps-ml-grid" class="ps-ml-grid">
        ${conn.links.length ? conn.links.map(mlCardHtml).join('')
          : `<tf-empty-state style="grid-column: 1 / -1;" icon="brain" title="${escapeAttr(t('ml_empty'))}"></tf-empty-state>`}
      </div>
    </tf-section-card>
  `;

  byId('ps-ml-create')?.addEventListener('click', () => openMlCreateWindow());
  byId('ps-ml-attach')?.addEventListener('click', () => openMlAttachWindow());

  const grid = byId('ps-ml-grid');
  grid?.addEventListener('click', (e) => {
    const more = e.target.closest('[data-ml-more]');
    if (more) {
      e.stopPropagation();
      more.parentElement?.querySelector('[data-ml-menu]')?.toggle();
      return;
    }
    const openBtn = e.target.closest('[data-ml-open]');
    if (openBtn && !openBtn.hasAttribute('disabled')) {
      const card = openBtn.closest('[data-ml-link]');
      const link = conn.links.find((l) => fv(l, 'link_id') === card?.dataset.mlLink);
      if (link) openMlStudio(link);
    }
  });
  grid?.addEventListener('action', async (e) => {
    const card = e.target.closest('[data-ml-link]');
    if (!card || !e.target.closest('[data-ml-menu]')) return;
    const link = conn.links.find((l) => fv(l, 'link_id') === card.dataset.mlLink);
    if (!link) return;
    switch (e.detail?.action) {
      case 'open': if (fv(link, 'can_open')) openMlStudio(link); break;
      case 'settings': openMlSettingsWindow(link); break;
      case 'sync': await syncMlLink(link); break;
      case 'detach': confirmDetachMlLink(link); break;
      default: break;
    }
  });
}

async function syncMlLink(link) {
  try {
    const resp = await ApiBinary.one('projectStudioMlLinkSyncNowRequest', {
      projectId: projectId(),
      linkId: fv(link, 'link_id'),
    });
    const outcome = resp.outcome || {};
    const errors = Array.isArray(outcome.errors) ? outcome.errors : [];
    const summary = t('ml_sync_result', {
      added: Number(fv(outcome, 'applied_add') ?? 0),
      updated: Number(fv(outcome, 'applied_update') ?? 0),
      removed: Number(fv(outcome, 'applied_remove') ?? 0),
      skipped: Number(outcome.skipped ?? 0),
    });
    toast(errors.length ? `${summary} — ${errors.join('; ')}` : summary, errors.length ? 'error' : 'success');
  } catch (err) {
    toast(`${t('ml_sync_failed')}: ${err.message}`, 'error');
  }
  await renderConnections();
}

function confirmDetachMlLink(link) {
  const name = fv(link, 'label') || (link.summary ? link.summary.name : fv(link, 'ml_project_id'));
  openDeleteWindow({
    title: t('ml_detach_title'),
    targetName: name,
    targetSub: t('ml_detach_sub'),
    targetIcon: 'brain',
    warning: t('ml_detach_warning'),
    extraHtml: `
      <div class="ps-toggle-inline" style="margin-top:10px;">
        <tf-checkbox id="ps-ml-revoke"></tf-checkbox>
        <span>${escapeHtml(t('ml_detach_revoke'))}</span>
      </div>
      <div class="ps-field-hint">${escapeHtml(t('ml_detach_revoke_hint'))}</div>
    `,
    confirmLabel: t('ml_detach_confirm'),
    onConfirm: async (dialogBody) => {
      const resp = await ApiBinary.one('projectStudioMlLinkDetachRequest', {
        projectId: projectId(),
        linkId: fv(link, 'link_id'),
        revokeMembers: !!dialogBody.querySelector('#ps-ml-revoke')?.checked,
      });
      toast(t('ml_detached_ok', { count: Number(fv(resp, 'members_removed') ?? 0) }), 'success');
      await renderConnections();
    },
  });
}

// The link maps explicit Knowledge levels onto ML Studio membership grants.
function roleMapHtml(roleMap) {
  return `
    <div class="ps-role-map">
      <div class="ps-role-map-head">
        <span>${escapeHtml(t('ml_permission_project'))}</span>
        <span>${escapeHtml(t('ml_role_ml'))}</span>
      </div>
      ${ML_PROJECT_LEVELS.map((role) => `
        <div class="ps-role-map-row">
          <span><tf-chip status="${role === 'admin' ? 'accent' : 'info'}">${escapeHtml(t(`access_level_${role}`))}</tf-chip></span>
          <tf-select data-role-map="${escapeAttr(role)}" value="${escapeAttr(roleMap[role] || '')}">
            <option value="">${escapeHtml(t('access_level_none'))}</option>
            ${(role === 'read' ? ['viewer'] : ML_ROLES).map((mlRole) => `<option value="${mlRole}" ${mlRole === roleMap[role] ? 'selected' : ''}>${escapeHtml(t(`ml_role_${mlRole}`))}</option>`).join('')}
          </tf-select>
        </div>
      `).join('')}
    </div><div class="ps-field-hint">${escapeHtml(t('ml_permission_hint'))}</div>
  `;
}

function wireRoleMap(body, roleMap) {
  body.querySelectorAll('[data-role-map]').forEach((sel) => {
    sel.addEventListener('change', (e) => {
      const value = e.detail?.value ?? sel.value ?? 'viewer';
      if (ML_ROLES.includes(value)) roleMap[sel.dataset.roleMap] = value;
      else delete roleMap[sel.dataset.roleMap];
    });
  });
}

function roleMapEntries(roleMap) {
  return ML_PROJECT_LEVELS.filter((level) => ML_ROLES.includes(roleMap[level])).map((level) => ({ projectRole: level, mlRole: roleMap[level] }));
}

function readRoleMap(link) {
  const map = {};
  const wire = Array.isArray(fv(link, 'role_map')) ? fv(link, 'role_map') : [];
  for (const entry of wire) {
    const role = fv(entry, 'project_role');
    const mlRole = fv(entry, 'ml_role');
    if (ML_PROJECT_LEVELS.includes(role) && ML_ROLES.includes(mlRole)) map[role] = mlRole;
  }
  return map;
}

function openMlCreateWindow() {
  const { body, foot, cleanup } = openWindow({
    title: t('ml_create_title'),
    subtitle: t('ml_create_sub'),
    icon: 'brain',
    width: 720,
  });
  const cw = { roleMap: { ...ML_DEFAULT_ROLE_MAP }, types: [], busy: false };

  body.innerHTML = `
    <div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('ml_permission_hint'))}</span></div>
    <div class="ps-ml-form-grid">
      <tf-input id="ps-ml-name" label="${escapeAttr(t('ml_name_label'))}"
        value="${escapeAttr(state.project?.name ? t('ml_name_default', { name: state.project.name }) : '')}"
        hint="${escapeAttr(t('ml_name_hint'))}"></tf-input>
      <div class="ps-field">
        <tf-select id="ps-ml-type" label="${escapeAttr(t('ml_type_label'))}" value=""></tf-select>
        <div class="ps-field-hint">${escapeHtml(t('ml_type_hint'))}</div>
      </div>
    </div>
    <div class="ps-banner-warn" data-ml-types-error hidden>${sprite('alert')}<span>${escapeHtml(t('ml_types_failed'))}</span></div>
    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('ml_permission_project'))}</span>
      ${roleMapHtml(cw.roleMap)}
    </div>
    <div class="ps-toggle-inline">
      <tf-toggle id="ps-ml-sync" checked></tf-toggle>
      <span>${escapeHtml(t('ml_sync_label'))}</span>
    </div>
    <div class="ps-field-hint">${escapeHtml(t('ml_permission_hint'))}</div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="primary" icon="check" data-action="create" disabled>${escapeHtml(t('ml_create'))}</tf-button>
    </div>
  `;
  wireRoleMap(body, cw.roleMap);

  const showError = (msg) => {
    const el = body.querySelector('[data-form-error]');
    if (el) { el.hidden = !msg; el.textContent = msg || ''; }
  };

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || cw.busy || btn.hasAttribute('disabled')) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    const mlName = String(body.querySelector('#ps-ml-name')?.value ?? '').trim();
    const projectType = String(body.querySelector('#ps-ml-type')?.value ?? '');
    if (mlName.length < 3) { showError(t('ml_err_name')); return; }
    if (!projectType) { showError(t('ml_err_type')); return; }
    showError(null);
    cw.busy = true;
    btn.setAttribute('disabled', '');
    try {
      const resp = await ApiBinary.one('projectStudioMlProjectCreateFromProjectRequest', {
        projectId: projectId(),
        mlName,
        projectType,
        roleMap: roleMapEntries(cw.roleMap),
        syncPermissions: !!body.querySelector('#ps-ml-sync')?.checked,
        label: mlName,
      });
      toast(t('ml_created_ok', {
        mapped: Number(fv(resp, 'members_mapped') ?? 0),
        skipped: Number(fv(resp, 'members_skipped') ?? 0),
      }), 'success');
      cleanup();
      await renderConnections();
    } catch (err) {
      cw.busy = false;
      btn.removeAttribute('disabled');
      showError(`${t('ml_create_failed')}: ${err.message}`);
    }
  });

  // The ML type catalogue is owned by ML Studio; without it there is nothing
  // valid to send, so creation stays locked instead of guessing a slug.
  (async () => {
    try {
      const resp = await ApiBinary.one('mlStudioProjectTypesListRequest');
      cw.types = Array.isArray(resp.types) ? resp.types : [];
    } catch {
      cw.types = [];
    }
    const banner = body.querySelector('[data-ml-types-error]');
    if (!cw.types.length) {
      if (banner) banner.hidden = false;
      return;
    }
    body.querySelector('#ps-ml-type')?.setOptions(
      cw.types.map((type) => ({ value: type.slug ?? type.id ?? '', label: type.label ?? type.name ?? type.slug ?? '' })),
      cw.types[0].slug ?? '',
    );
    foot.querySelector('[data-action="create"]')?.removeAttribute('disabled');
  })();
}

function openMlAttachWindow() {
  const { body, foot, cleanup } = openWindow({
    title: t('ml_attach_title'),
    subtitle: t('ml_attach_sub'),
    icon: 'share',
    width: 720,
  });
  // sync_permissions defaults OFF for an existing ML project: its member list
  // predates the link and must not be rewritten without an explicit decision.
  const aw = { roleMap: { ...ML_DEFAULT_ROLE_MAP }, candidates: [], mlProjectId: '', busy: false };

  body.innerHTML = `
    <div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('ml_attach_banner'))}</span></div>
    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('ml_attach_pick_label'))}</span>
      <tf-combobox id="ps-ml-candidate" placeholder="${escapeAttr(t('ml_attach_pick_placeholder'))}"></tf-combobox>
      <div class="ps-field-hint">${escapeHtml(t('ml_attach_pick_hint'))}</div>
    </div>
    <tf-input id="ps-ml-label" label="${escapeAttr(t('ml_label_label'))}" hint="${escapeAttr(t('ml_label_hint'))}"></tf-input>
    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('ml_permission_project'))}</span>
      ${roleMapHtml(aw.roleMap)}
    </div>
    <div class="ps-toggle-inline">
      <tf-toggle id="ps-ml-attach-sync"></tf-toggle>
      <span>${escapeHtml(t('ml_sync_label'))}</span>
    </div>
    <div class="ps-field-hint">${escapeHtml(t('ml_attach_sync_hint'))}</div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="primary" icon="check" data-action="attach">${escapeHtml(t('ml_attach_confirm'))}</tf-button>
    </div>
  `;
  wireRoleMap(body, aw.roleMap);

  const showError = (msg) => {
    const el = body.querySelector('[data-form-error]');
    if (el) { el.hidden = !msg; el.textContent = msg || ''; }
  };
  body.querySelector('#ps-ml-candidate')?.addEventListener('change', (e) => {
    aw.mlProjectId = e.detail?.value || '';
  });

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || aw.busy) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    if (!aw.mlProjectId) { showError(t('ml_err_candidate')); return; }
    showError(null);
    aw.busy = true;
    btn.setAttribute('disabled', '');
    try {
      await ApiBinary.one('projectStudioMlLinkAttachRequest', {
        projectId: projectId(),
        mlProjectId: aw.mlProjectId,
        label: String(body.querySelector('#ps-ml-label')?.value ?? '').trim(),
        syncPermissions: !!body.querySelector('#ps-ml-attach-sync')?.checked,
        roleMap: roleMapEntries(aw.roleMap),
      });
      toast(t('ml_attached_ok'), 'success');
      cleanup();
      await renderConnections();
    } catch (err) {
      aw.busy = false;
      btn.removeAttribute('disabled');
      showError(`${t('ml_attach_failed')}: ${err.message}`);
    }
  });

  (async () => {
    let candidates = [];
    try {
      const resp = await ApiBinary.one('projectStudioMlProjectCandidatesRequest', { projectId: projectId() });
      candidates = Array.isArray(resp.candidates) ? resp.candidates : [];
    } catch (err) {
      showError(`${t('ml_candidates_failed')}: ${err.message}`);
    }
    aw.candidates = candidates;
    const box = body.querySelector('#ps-ml-candidate');
    if (!box) return;
    box.options = candidates.map((c) => ({
      value: fv(c, 'ml_project_id'),
      label: c.name,
      description: fv(c, 'project_type_label') || fv(c, 'project_type') || '',
    }));
    if (!candidates.length) showError(t('ml_candidates_empty'));
  })();
}

function openMlSettingsWindow(link) {
  const { body, foot, cleanup } = openWindow({
    title: t('ml_settings_title'),
    subtitle: fv(link, 'label') || (link.summary ? link.summary.name : fv(link, 'ml_project_id')),
    icon: 'settings',
    width: 680,
  });
  const uw = { roleMap: readRoleMap(link), busy: false };

  body.innerHTML = `
    <tf-input id="ps-ml-set-label" label="${escapeAttr(t('ml_label_label'))}" value="${escapeAttr(fv(link, 'label') || '')}"
      hint="${escapeAttr(t('ml_label_hint'))}"></tf-input>
    <div class="ps-case-info-list">
      <div><span>${escapeHtml(t('ml_info_origin'))}</span><b>${escapeHtml(t(`ml_origin_${fv(link, 'origin')}`))}</b></div>
      <div><span>${escapeHtml(t('ml_info_created_by'))}</span><b>${escapeHtml(fv(link, 'created_by_name') || '—')}</b></div>
      <div><span>${escapeHtml(t('ml_info_last_sync'))}</span><b>${escapeHtml(fv(link, 'last_sync_at') ? formatTimestamp(fv(link, 'last_sync_at')) : '—')}</b></div>
      <div><span>${escapeHtml(t('ml_info_last_sync_result'))}</span><b>${escapeHtml(fv(link, 'last_sync_result') || '—')}</b></div>
    </div>
    <div class="ps-field">
      <span class="ps-field-label">${escapeHtml(t('ml_permission_project'))}</span>
      ${roleMapHtml(uw.roleMap)}
    </div>
    <div class="ps-toggle-inline">
      <tf-toggle id="ps-ml-set-sync" ${fv(link, 'sync_permissions') ? 'checked' : ''}></tf-toggle>
      <span>${escapeHtml(t('ml_sync_label'))}</span>
    </div>
    <div class="ps-field-hint">${escapeHtml(t('ml_permission_hint'))}</div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="primary" icon="check" data-action="save">${escapeHtml(t('action_save'))}</tf-button>
    </div>
  `;
  wireRoleMap(body, uw.roleMap);

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || uw.busy) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    uw.busy = true;
    btn.setAttribute('disabled', '');
    try {
      await ApiBinary.one('projectStudioMlLinkUpdateRequest', {
        projectId: projectId(),
        linkId: fv(link, 'link_id'),
        label: String(body.querySelector('#ps-ml-set-label')?.value ?? '').trim(),
        syncPermissions: !!body.querySelector('#ps-ml-set-sync')?.checked,
        roleMap: roleMapEntries(uw.roleMap),
      });
      toast(t('ml_settings_saved'), 'success');
      cleanup();
      await renderConnections();
    } catch (err) {
      uw.busy = false;
      btn.removeAttribute('disabled');
      const el = body.querySelector('[data-form-error]');
      if (el) { el.hidden = false; el.textContent = `${t('ml_settings_failed')}: ${err.message}`; }
    }
  });
}

// =============================================================================
// Z01 (F4) — kanban task board
// =============================================================================

function canMoveTask(task) {
  return !task.archived_at && allowsArea(taskSourceProject(task)?.access, 'board', 'write');
}

// Card order is NOT persisted in F4 — the board sorts by priority and then by
// last change, so a reload always produces the same, explainable order.
function boardSortedTasks(rows) {
  const rank = { critical: 0, high: 1, medium: 2, low: 3 };
  return rows.slice().sort((a, b) => {
    const byPriority = (rank[a.priority] ?? 9) - (rank[b.priority] ?? 9);
    if (byPriority !== 0) return byPriority;
    return String(fv(b, 'updated_at') || '').localeCompare(String(fv(a, 'updated_at') || ''));
  });
}

function taskCardModel(task) {
  const type = fv(task, 'task_type');
  const severity = task.severity || '';
  const dueDate = fv(task, 'due_date') || '';
  const assignee = fv(task, 'assigned_to_name') || '';
  const links = (() => {
    try {
      const parsed = JSON.parse(fv(task, 'links_json') || '[]');
      return Array.isArray(parsed) ? parsed : [];
    } catch {
      return [];
    }
  })();
  const meta = [];
  if (state.tasksView.scope === 'descendants') meta.push({ text: task.project_name, icon: 'folder' });
  meta.push({ text: taskRowTypeName(task) });
  if (task.resolution === 'not_pursued') meta.push({ text: t('task_not_pursued'), tone: 'info' });
  if (task.archived_at) meta.push({ text: t('task_archived'), tone: 'warning', icon: 'archive' });
  if (severity) {
    meta.push({
      text: t(`sev_${severity}`),
      tone: { critical: 'danger', high: 'warning', medium: 'accent', low: 'info' }[severity] || 'neutral',
    });
  }
  meta.push({ text: t(`prio_${task.priority}`), icon: 'trend' });
  if (links.length) meta.push({ text: t('board_links', { count: links.length }), icon: 'branch' });
  if (Number(fv(task, 'comment_count') ?? 0) > 0) {
    meta.push({ text: String(fv(task, 'comment_count')), icon: 'message' });
  }

  const menu = [{ id: 'open', label: t('action_open'), icon: 'external-link' }];
  if (allowsArea(taskSourceProject(task)?.access, 'tasks', 'write')) {
    menu.push({ id: task.archived_at ? 'restore' : 'archive', label: t(task.archived_at ? 'task_restore' : 'action_archive'), icon: task.archived_at ? 'refresh' : 'archive' });
  }

  return {
    id: fv(task, 'task_id'),
    column: task.status,
    title: task.title,
    badge: taskNoLabel(task),
    badgeKind: type === 'defect' ? 'danger' : 'info',
    badgeIcon: type === 'defect' ? 'alert' : 'check',
    accent: severity === 'critical' ? 'danger' : null,
    disabled: !canMoveTask(task),
    meta,
    footer: {
      left: dueDate ? { icon: 'clock', text: dueDate } : null,
      right: assignee
        ? { avatar: true, text: initials(assignee), title: assignee }
        : { avatar: true, text: '?', title: t('board_unassigned') },
    },
    menu,
  };
}

function renderTaskBoard() {
  const tv = state.tasksView;
  const host = byId('ps-tasks-table-host');
  if (!host || !tv) return;

  const board = document.createElement('tf-kanban');
  board.setAttribute('empty-text', t('board_column_empty'));
  board.labels = {
    empty: t('board_column_empty'),
    dragHint: t('board_drag_hint'),
    countLabel: '{n}',
    limitLabel: '{n}/{limit}',
    cardsLabel: t('board_cards_label'),
    limitExceeded: t('board_limit_exceeded'),
    addCard: t('board_add_card'),
    cardMenu: t('action_more'),
    grabbed: t('board_a11y_grabbed'),
    moved: t('board_a11y_moved'),
    dropped: t('board_a11y_dropped'),
    cancelled: t('board_a11y_cancelled'),
  };
  board.columns = TASK_BOARD_COLUMNS.map((col) => ({
    id: col.id,
    label: t(`task_status_${col.id}`),
    accent: col.accent, count: tv.summary[col.id],
  }));
  board.readOnly = !descendantsOf(projectId()).some((project) => allowsArea(project.access, 'board', 'write'));
  board.cards = boardSortedTasks(tv.boardRows).map(taskCardModel);

  board.addEventListener('card-open', (e) => {
    const taskId = e.detail?.cardId;
    const task = tv.boardRows.find((row) => row.task_id === taskId);
    if (task) openTaskRecord(task);
  });
  board.addEventListener('column-add', (e) => {
    if (!canCreateTask(projectAccess())) return;
    openTaskWindow({ status: e.detail?.columnId });
  });
  board.addEventListener('card-menu', async (e) => {
    const taskId = e.detail?.cardId;
    const task = tv.boardRows.find((row) => fv(row, 'task_id') === taskId);
    if (!task) return;
    if (e.detail?.actionId === 'open') { openTaskRecord(task); return; }
    if (e.detail?.actionId === 'archive') confirmArchiveBoardTask(task);
    if (e.detail?.actionId === 'restore' && allowsArea(taskSourceProject(task)?.access, 'tasks', 'write')) setTaskArchived(taskId, false, task.project_id).catch((error) => toast(`${t('task_archive_failed')}: ${error.message}`, 'error'));
  });
  // The board already shows the new position when this fires; a rejected write
  // is undone with revertMove so the UI never drifts from the server.
  board.addEventListener('card-move', async (e) => {
    const { cardId, to } = e.detail || {};
    const task = tv.boardRows.find((row) => fv(row, 'task_id') === cardId);
    if (!task || !to || task.status === to) return;
    if (!canMoveTask(task)) { board.revertMove(cardId); return; }
    const previous = task.status;
    task.status = to;
    try {
      await ApiBinary.one('projectStudioTaskStatusSetRequest', {
        projectId: task.project_id,
        taskId: cardId,
        status: to,
      });
      await loadTasksBoard();
      renderTaskBoard(); renderTaskIndexState();
    } catch (err) {
      task.status = previous;
      board.revertMove(cardId);
      toast(`${t('board_move_failed')}: ${err.message}`, 'error');
    }
  });

  host.replaceChildren(board);
  syncTasksFooter();
  if (tv.boardRows.length < tv.total) {
    const more = document.createElement('tf-button'); more.setAttribute('variant', 'ghost'); more.textContent = t('activity_more');
    more.addEventListener('click', async () => { more.setAttribute('disabled', ''); try { await loadTasksBoard(true); renderTaskBoard(); renderTaskIndexState(); } catch (error) { more.removeAttribute('disabled'); toast(`${t('tasks_failed')}: ${error.message}`, 'error'); } });
    host.appendChild(more);
  }
}

async function confirmArchiveBoardTask(task) {
  if (!allowsArea(taskSourceProject(task)?.access, 'tasks', 'write')) return;
  const confirmed = await TfWindow.confirm({ title: t('task_archive_title'), message: t('task_archive_message', { key: taskNoLabel(task), title: task.title }), confirmLabel: t('action_archive'), cancelLabel: t('action_cancel') });
  if (!confirmed) return;
  try { await setTaskArchived(fv(task, 'task_id'), true, task.project_id); }
  catch (error) { toast(`${t('task_archive_failed')}: ${error.message}`, 'error'); }
}

// =============================================================================
// Project export / import archives
// =============================================================================

function stopArchiveJob() {
  const job = state.archiveJob;
  if (!job) return;
  if (job.unsub) { try { job.unsub(); } catch { /* stream already gone */ } }
  if (job.pollTimer) clearInterval(job.pollTimer);
  state.archiveJob = null;
}

// One live archive job at a time. The stream only feeds the log; the status
// poll stays the source of truth for progress and the terminal outcome.
function trackArchiveJob(jobId, kind, { onStatus, onLog }) {
  stopArchiveJob();
  const job = { jobId, kind, unsub: null, pollTimer: null, log: [] };
  state.archiveJob = job;

  ApiBinary.subscribe(
    'projectStudioArchiveStreamRequest',
    { jobId },
    {
      onChunk: (body) => {
        if (body?.variant !== 'ProjectStudioArchiveStreamChunk') return;
        if (state.archiveJob !== job) return;
        const line = body.phase ? `[${body.phase}] ${body.line}` : String(body.line || '');
        job.log.push(line);
        if (job.log.length > ARCHIVE_LOG_CAP) job.log.splice(0, job.log.length - ARCHIVE_LOG_CAP);
        onLog(job.log);
      },
      onError: () => { /* the poll below is the source of truth */ },
      onEnd: () => { /* terminal state is confirmed by the poll */ },
    },
  ).then((unsub) => {
    if (state.archiveJob !== job) { unsub(); return; }
    job.unsub = unsub;
  }).catch(() => { /* the stream is optional, polling still tracks the job */ });

  const poll = async () => {
    if (state.archiveJob !== job) return;
    let status = null;
    try {
      status = kind === 'export'
        ? await ApiBinary.one('projectStudioProjectExportStatusRequest', { projectId: projectId(), jobId })
        : await ApiBinary.one('projectStudioProjectImportStatusRequest', { jobId });
    } catch {
      return;
    }
    if (state.archiveJob !== job) return;
    if (String(status.status || '') !== 'running') stopArchiveJob();
    await onStatus(status);
  };
  job.pollTimer = setInterval(poll, ARCHIVE_POLL_MS);
  poll();
}

function inventoryHtml(inventory, totalBytes) {
  if (!inventory) return '';
  const cell = (labelKey, value) => `
    <div class="ps-inv-cell"><span>${escapeHtml(t(labelKey))}</span><b>${escapeHtml(String(value))}</b></div>
  `;
  return `
    <div class="ps-inventory">
      ${cell('inv_cases', Number(inventory.cases ?? 0))}
      ${cell('inv_suites', Number(inventory.suites ?? 0))}
      ${cell('inv_runs', Number(inventory.runs ?? 0))}
      ${cell('inv_tasks', Number(inventory.tasks ?? 0))}
      ${cell('inv_documents', Number(inventory.documents ?? 0))}
      ${cell('inv_sources', Number(inventory.sources ?? 0))}
      ${cell('inv_files', `${Number(inventory.files ?? 0)} · ${formatBytes(Number(fv(inventory, 'bytes_files') ?? 0))}`)}
      ${cell('inv_run_artifacts', formatBytes(Number(fv(inventory, 'bytes_runs') ?? 0)))}
      ${cell('inv_vectors', `${Number(inventory.vectors ?? 0)} · ${Number(fv(inventory, 'vector_dim') ?? 0)}d`)}
      ${cell('inv_embedding', fv(inventory, 'embedding_model') || fv(inventory, 'embedding_alias') || '—')}
      ${totalBytes != null ? cell('inv_total_bytes', formatBytes(Number(totalBytes))) : ''}
    </div>
  `;
}

function openExportWindow() {
  const { body, foot, cleanup } = openWindow({
    title: t('export_title'),
    subtitle: state.project?.name || '',
    icon: 'download',
    width: 680,
  });
  if (!canProject('export')) { cleanup(); return; }
  const ew = { scope: 'node', jobId: null, signedUrl: '', busy: false };

  body.innerHTML = `
    <div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('export_intro'))}</span></div>
    <tf-select id="ps-export-scope" label="${escapeAttr(t('export_scope'))}" value="node"><option value="node">${escapeHtml(t('scope_node'))}</option><option value="subtree">${escapeHtml(t('scope_subtree'))}</option></tf-select>
    <div data-export-nodes></div>
    <div class="ps-export-options">
      <div class="ps-toggle-inline">
        <tf-checkbox id="ps-exp-runs" checked></tf-checkbox>
        <span>${escapeHtml(t('export_include_runs'))}</span>
      </div>
      <div class="ps-field-hint">${escapeHtml(t('export_include_runs_hint'))}</div>
      <div class="ps-toggle-inline">
        <tf-checkbox id="ps-exp-vectors" checked></tf-checkbox>
        <span>${escapeHtml(t('export_include_vectors'))}</span>
      </div>
      <div class="ps-field-hint">${escapeHtml(t('export_include_vectors_hint'))}</div>
      <div class="ps-toggle-inline">
        <tf-checkbox id="ps-exp-names"></tf-checkbox>
        <span>${escapeHtml(t('export_include_names'))}</span>
      </div>
      <div class="ps-field-hint">${escapeHtml(t('export_include_names_hint'))}</div>
    </div>
    <div class="ps-banner-warn">${sprite('alert')}<span>${escapeHtml(t('export_secrets_note'))}</span></div>
    <div data-export-progress hidden>
      <tf-progress-bar id="ps-exp-bar" value="0" label="${escapeAttr(t('export_phase_start'))}"></tf-progress-bar>
      <div class="ps-archive-phase" data-export-phase></div>
      <pre class="ps-archive-log" data-export-log></pre>
    </div>
    <div data-export-result hidden></div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="close-export">${escapeHtml(t('action_close'))}</tf-button>
      <tf-button variant="primary" icon="download" data-action="start" disabled>${escapeHtml(t('export_start'))}</tf-button>
    </div>
  `;

  const showError = (msg) => {
    const el = body.querySelector('[data-form-error]');
    if (el) { el.hidden = !msg; el.textContent = msg || ''; }
  };

  let scopeGeneration = 0;
  const previewScope = async () => {
    const query = ++scopeGeneration;
    const button = foot.querySelector('[data-action="start"]'); button.setAttribute('disabled', '');
    ew.scope = body.querySelector('#ps-export-scope').value;
    try {
      const response = await ApiBinary.one('projectStudioProjectScopePreviewRequest', { projectId: projectId(), scope: ew.scope, operation: 'export' });
      if (query !== scopeGeneration || !body.isConnected) return;
      body.querySelector('[data-export-nodes]').innerHTML = scopeNodesHtml(response.nodes); button.removeAttribute('disabled');
    } catch (error) { showError(error.message); }
  };
  body.querySelector('#ps-export-scope').addEventListener('change', previewScope); previewScope();

  const onStatus = (status) => {
    const pct = Number(fv(status, 'progress_pct') ?? 0);
    body.querySelector('#ps-exp-bar')?.setAttribute('value', String(pct));
    const phase = body.querySelector('[data-export-phase]');
    if (phase) phase.textContent = t('export_phase', { phase: status.phase || '—', pct });
    const state_ = String(status.status || '');
    if (state_ === 'running') return;
    const result = body.querySelector('[data-export-result]');
    if (!result) return;
    result.hidden = false;
    if (state_ !== 'success') {
      result.innerHTML = `<div class="ps-form-error">${escapeHtml(status.error || t('export_failed'))}</div>`;
      foot.querySelector('[data-action="start"]')?.removeAttribute('disabled');
      ew.busy = false;
      return;
    }
    ew.signedUrl = fv(status, 'signed_url') || '';
    result.innerHTML = `
      <div class="ps-banner-info">${sprite('check')}<span>${escapeHtml(t('export_done', { size: formatBytes(Number(fv(status, 'archive_bytes') ?? 0)) }))}</span></div>
      ${inventoryHtml(status.inventory, null)}
      <div class="ps-export-download">
        <tf-button variant="primary" icon="download" data-action="download">${escapeHtml(t('export_download'))}</tf-button>
      </div>
    `;
    foot.querySelector('[data-action="start"]')?.setAttribute('hidden', '');
  };

  body.addEventListener('click', (e) => {
    if (!e.target.closest('[data-action="download"]')) return;
    if (!ew.signedUrl) { showError(t('export_no_url')); return; }
    // The archive is downloaded over its signed HTTPS URL, not the binary
    // protocol: it can reach tens of gigabytes.
    downloadUrl(ew.signedUrl, `${state.project?.name || 'project'}.tfproj.zip`);
  });

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'close-export') { stopArchiveJob(); cleanup(); return; }
    if (btn.dataset.action !== 'start' || ew.busy) return;
    showError(null);
    ew.busy = true;
    btn.setAttribute('disabled', '');
    try {
      const resp = await ApiBinary.one('projectStudioProjectExportStartRequest', {
        projectId: projectId(),
        scope: ew.scope,
        includeRuns: !!body.querySelector('#ps-exp-runs')?.checked,
        includeVectors: !!body.querySelector('#ps-exp-vectors')?.checked,
        includeUserNames: !!body.querySelector('#ps-exp-names')?.checked,
      });
      ew.jobId = fv(resp, 'job_id');
      body.querySelector('#ps-export-scope').setAttribute('disabled', '');
      body.querySelector('[data-export-progress]').hidden = false;
      trackArchiveJob(ew.jobId, 'export', {
        onStatus,
        onLog: (lines) => {
          const log = body.querySelector('[data-export-log]');
          if (log) { log.textContent = lines.join('\n'); log.scrollTop = log.scrollHeight; }
        },
      });
    } catch (err) {
      ew.busy = false;
      btn.removeAttribute('disabled');
      showError(`${t('export_start_failed')}: ${err.message}`);
    }
  });
}

function openImportWindow() {
  const { body, foot, cleanup } = openWindow({
    title: t('import_title'),
    subtitle: t('import_sub'),
    icon: 'cloud',
    width: 760,
  });
  // controls.css forces display:block on tf-progress-bar, which beats the
  // [hidden] UA rule — the upload bar is toggled through its wrapper div.
  const iw = { file: null, uploadId: '', preview: null, jobId: null, busy: false };

  const showError = (msg) => {
    const el = body.querySelector('[data-form-error]');
    if (el) { el.hidden = !msg; el.textContent = msg || ''; }
  };
  const setStep = (step) => {
    body.querySelectorAll('[data-import-step]').forEach((el) => {
      el.hidden = el.dataset.importStep !== step;
    });
    foot.querySelectorAll('[data-step-action]').forEach((el) => {
      el.hidden = el.dataset.stepAction !== step;
    });
  };

  body.innerHTML = `
    <div data-import-step="pick">
      <div class="ps-banner-info">${sprite('info')}<span>${escapeHtml(t('import_intro'))}</span></div>
      <tf-file-input id="ps-imp-file" accept=".zip" label="${escapeAttr(t('import_dropzone'))}"></tf-file-input>
      <div data-import-file></div>
      <div data-import-upload hidden>
        <tf-progress-bar id="ps-imp-upload-bar" value="0" label="${escapeAttr(t('import_uploading'))}"></tf-progress-bar>
      </div>
    </div>
    <div data-import-step="preview" hidden></div>
    <div data-import-step="apply" hidden>
      <tf-progress-bar id="ps-imp-bar" value="0" label="${escapeAttr(t('import_phase_start'))}"></tf-progress-bar>
      <div class="ps-archive-phase" data-import-phase></div>
      <pre class="ps-archive-log" data-import-log></pre>
      <div data-import-result></div>
    </div>
    <div class="ps-form-error" data-form-error hidden></div>
  `;
  foot.innerHTML = `
    <div class="ps-footer-left"></div>
    <div class="ps-footer-right">
      <tf-button variant="ghost" data-action="close-import">${escapeHtml(t('action_close'))}</tf-button>
      <tf-button variant="primary" icon="arrow" data-action="upload" data-step-action="pick" disabled>${escapeHtml(t('import_analyze'))}</tf-button>
      <tf-button variant="primary" icon="check" data-action="apply" data-step-action="preview" hidden>${escapeHtml(t('import_apply'))}</tf-button>
    </div>
  `;
  setStep('pick');

  body.querySelector('#ps-imp-file')?.addEventListener('change', (e) => {
    const file = e.detail?.files?.[0] ?? null;
    iw.file = file;
    const slot = body.querySelector('[data-import-file]');
    if (slot) {
      slot.innerHTML = file ? `
        <div class="ps-added-file">
          <span class="ps-af-ico">${sprite('folder')}</span>
          <div class="ps-af-main">
            <div class="ps-af-name">${escapeHtml(file.name)}</div>
            <div class="ps-af-size">${escapeHtml(formatBytes(file.size))}</div>
          </div>
        </div>
      ` : '';
    }
    const btn = foot.querySelector('[data-action="upload"]');
    if (file) btn?.removeAttribute('disabled');
    else btn?.setAttribute('disabled', '');
  });

  const uploadArchive = async () => {
    const uploadId = crypto.randomUUID();
    const totalChunks = Math.max(1, Math.ceil(iw.file.size / IMPORT_CHUNK_BYTES));
    const bar = body.querySelector('#ps-imp-upload-bar');
    const barWrap = body.querySelector('[data-import-upload]');
    if (barWrap) barWrap.hidden = false;
    // Archives run to gigabytes, so each slice is materialised on its own: reading
    // the whole File into one buffer would put the entire archive in the tab's heap.
    for (let seq = 0; seq < totalChunks; seq += 1) {
      const start = seq * IMPORT_CHUNK_BYTES;
      const slice = iw.file.slice(start, Math.min(start + IMPORT_CHUNK_BYTES, iw.file.size));
      const chunk = new Uint8Array(await slice.arrayBuffer());
      await ApiBinary.one('projectStudioProjectImportUploadChunkRequest', {
        uploadId,
        filename: iw.file.name,
        seq,
        totalChunks,
        bytes: chunk,
      });
      bar?.setAttribute('value', String(Math.round(((seq + 1) / totalChunks) * 100)));
    }
    return uploadId;
  };

  const renderPreview = (preview) => {
    const host = body.querySelector('[data-import-step="preview"]');
    if (!host) return;
    const modules = Array.isArray(preview.modules) ? preview.modules : [];
    const reusable = !!fv(preview, 'vectors_reusable');
    host.innerHTML = `
      <div class="ps-preview-head">
        <div class="ps-preview-name">${escapeHtml(fv(preview, 'project_name') || '')}</div>
        <div class="ps-preview-sub">${escapeHtml(t('import_preview_sub', {
          exported: formatTimestamp(fv(preview, 'exported_at')),
          version: Number(fv(preview, 'archive_version') ?? 0),
        }))}</div>
        <div class="ps-preview-chips">
          <tf-chip status="info">${escapeHtml(orientationLabel({ template: preview.template, modules: preview.modules }))}</tf-chip>
          ${modules.map((m) => `<tf-chip>${escapeHtml(t(`module_${m}`))}</tf-chip>`).join('')}
        </div>
      </div>
      <tf-section-card title="${escapeAttr(t('import_tree_title'))}" icon="branch"><div class="ps-field-hint">${escapeHtml(t('import_prefix_allocation'))}</div><tf-table data-import-tree><tf-column key="name" label="${escapeAttr(t('table_col_project'))}"></tf-column><tf-column key="prefix" label="${escapeAttr(t('import_source_prefix'))}"></tf-column><tf-column key="visibility" label="${escapeAttr(t('subproject_private'))}"></tf-column><tf-column key="status" label="${escapeAttr(t('table_col_status'))}"></tf-column><tf-column key="tasks" label="${escapeAttr(t('inv_tasks'))}" renderer="num"></tf-column></tf-table></tf-section-card>
      ${inventoryHtml(preview.inventory, fv(preview, 'total_uncompressed_bytes'))}
      <div class="ps-banner-${reusable ? 'info' : 'warn'}">
        ${sprite(reusable ? 'info' : 'alert')}
        <span>${escapeHtml(reusable
          ? t('import_vectors_reusable')
          : t('import_vectors_reindex', { reason: fv(preview, 'vectors_reason') || '—' }))}</span>
      </div>
      <div class="ps-banner-warn">${sprite('alert')}<span>${escapeHtml(t('import_env_warning'))}</span></div>
      <div class="ps-banner-warn">${sprite('alert')}<span>${escapeHtml(t('import_sources_warning'))}</span></div>
      <tf-input id="ps-imp-name" label="${escapeAttr(t('import_name_label'))}" value="${escapeAttr(fv(preview, 'project_name') || '')}"
        hint="${escapeAttr(t('import_name_hint'))}"></tf-input>
      <div class="ps-toggle-inline">
        <tf-checkbox id="ps-imp-vectors" ${reusable ? 'checked' : ''}></tf-checkbox>
        <span>${escapeHtml(t('import_take_vectors'))}</span>
      </div>
      <div class="ps-toggle-inline">
        <tf-checkbox id="ps-imp-runs" ${fv(preview, 'has_runs') ? 'checked' : ''} ${fv(preview, 'has_runs') ? '' : 'disabled'}></tf-checkbox>
        <span>${escapeHtml(t('import_take_runs'))}</span>
      </div>
    `;
    const nodes = new Map(preview.tree_nodes.map((row) => [row.project_id, row]));
    const nodePath = (row) => {
      const names = [row.name];
      let ancestor = nodes.get(row.parent_id);
      for (let level = 1; ancestor && level < 4; level++) { names.unshift(ancestor.name); ancestor = nodes.get(ancestor.parent_id); }
      return names.join(' / ');
    };
    const treeTable = host.querySelector('[data-import-tree]');
    treeTable.rowKey = '_id';
    treeTable.rows = preview.tree_nodes.map((row) => ({ _id: row.project_id, name: nodePath(row), prefix: row.key_prefix, visibility: row.is_private ? t('subproject_private') : '—', status: t(row.lifecycle === 'ended' ? 'status_ended' : 'status_active'), tasks: row.inventory.tasks }));
    const detail = document.createElement('div'); treeTable.after(detail);
    treeTable.addEventListener('row-click', (event) => { const row = preview.tree_nodes.find((node) => node.project_id === event.detail.row._id); detail.innerHTML = `<h3>${escapeHtml(row.name)}</h3>${inventoryHtml(row.inventory, null)}`; });
  };

  const onStatus = async (status) => {
    const pct = Number(fv(status, 'progress_pct') ?? 0);
    body.querySelector('#ps-imp-bar')?.setAttribute('value', String(pct));
    const phase = body.querySelector('[data-import-phase]');
    if (phase) phase.textContent = t('import_phase', { phase: status.phase || '—', pct });
    const state_ = String(status.status || '');
    if (state_ === 'running') return;
    iw.busy = false;
    const result = body.querySelector('[data-import-result]');
    if (!result) return;
    if (state_ !== 'success') {
      result.innerHTML = `<div class="ps-form-error">${escapeHtml(status.error || t('import_failed'))}</div>`;
      return;
    }
    const newProjectId = fv(status, 'project_id');
    const reindexJobs = Array.isArray(fv(status, 'reindex_job_ids')) ? fv(status, 'reindex_job_ids') : [];
    result.innerHTML = `
      <div class="ps-banner-info">${sprite('check')}<span>${escapeHtml(fv(status, 'vectors_imported')
        ? t('import_done_vectors')
        : t('import_done_reindex', { count: reindexJobs.length }))}</span></div>
      <div class="ps-export-download">
        <tf-button variant="primary" icon="external-link" data-action="open-imported">${escapeHtml(t('import_open_project'))}</tf-button>
      </div>
      <tf-section-card title="${escapeAttr(t('import_saved_tree'))}" icon="branch"><div data-imported-tree-status class="ps-field-hint">${escapeHtml(t('loading'))}</div><tf-table data-imported-tree hidden><tf-column key="name" label="${escapeAttr(t('table_col_project'))}"></tf-column><tf-column key="prefix" label="${escapeAttr(t('project_key_prefix'))}"></tf-column></tf-table></tf-section-card>
    `;
    result.querySelector('[data-action="open-imported"]')?.addEventListener('click', async () => {
      stopArchiveJob();
      cleanup();
      await loadProjects();
      if (newProjectId) await openProject(newProjectId);
    });
    try {
      const tree = await ApiBinary.one('projectStudioProjectTreeRequest', { projectId: newProjectId, includeEnded: true, includeArchived: true });
      if (!result.isConnected) return;
      const table = result.querySelector('[data-imported-tree]');
      table.rowKey = '_id';
      table.rows = tree.projects.map((project) => ({ _id: project.project_id, name: [...projectAncestors(project, tree.projects, tree.breadcrumbs).map((ancestor) => ancestor.name), project.name].join(' / '), prefix: project.key_prefix }));
      table.hidden = false;
      result.querySelector('[data-imported-tree-status]').remove();
      await loadProjects();
    } catch (error) {
      const hint = result.querySelector('[data-imported-tree-status]');
      if (hint && result.isConnected) hint.textContent = `${t('load_failed')}: ${error.message}`;
    }
  };

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || iw.busy) return;
    if (btn.dataset.action === 'close-import') { stopArchiveJob(); cleanup(); return; }

    if (btn.dataset.action === 'upload') {
      if (!iw.file) { showError(t('import_err_file')); return; }
      showError(null);
      iw.busy = true;
      btn.setAttribute('disabled', '');
      try {
        iw.uploadId = await uploadArchive();
        // The preview reads ONLY the manifest — nothing is unpacked before the
        // operator confirms.
        iw.preview = await ApiBinary.one('projectStudioProjectImportPreviewRequest', { uploadId: iw.uploadId });
        renderPreview(iw.preview);
        setStep('preview');
      } catch (err) {
        showError(`${t('import_preview_failed')}: ${err.message}`);
      }
      iw.busy = false;
      btn.removeAttribute('disabled');
      return;
    }

    if (btn.dataset.action !== 'apply') return;
    showError(null);
    iw.busy = true;
    btn.setAttribute('disabled', '');
    try {
      const resp = await ApiBinary.one('projectStudioProjectImportApplyRequest', {
        uploadId: iw.uploadId,
        nameOverride: String(body.querySelector('#ps-imp-name')?.value ?? '').trim(),
        importVectors: !!body.querySelector('#ps-imp-vectors')?.checked,
        importRuns: !!body.querySelector('#ps-imp-runs')?.checked,
      });
      iw.jobId = fv(resp, 'job_id');
      setStep('apply');
      trackArchiveJob(iw.jobId, 'import', {
        onStatus,
        onLog: (lines) => {
          const log = body.querySelector('[data-import-log]');
          if (log) { log.textContent = lines.join('\n'); log.scrollTop = log.scrollHeight; }
        },
      });
    } catch (err) {
      iw.busy = false;
      btn.removeAttribute('disabled');
      showError(`${t('import_apply_failed')}: ${err.message}`);
    }
  });
}
