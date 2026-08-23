// ===== File: agents.js — Agents module: card list, config-first agent
// detail (breadcrumb → header → tabs), create-only wizard =====
//
// Admin/user surface over the `agents` registry — all over the binary protocol
// (`MessageBody::AgentsBody` via ApiBinary + codec helpers, never REST).
// Opening an agent lands on its DETAIL view (mockups/agenci-20260822/A02):
// breadcrumb → detail-header → underline tabs Konfiguracja / Narzędzia i
// umiejętności / Testowanie / Przebiegi. Configuration is edited inline in the
// tab with a sticky save bar; the wizard window is CREATE-only and the AI
// builder assistant feeds both the wizard and the open draft. The playground
// (chat + live run panel) lives in the Testowanie tab, per-agent run history
// in the Przebiegi tab; the top-level Runs tab stays the cross-agent view.
// tf-* components only.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { ModelModalities } from '/js/modules/flows-builder/model-modalities.js';
import { TfWindow } from '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-table.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-menu.js';
import '/js/components/tf-slider.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-chat-bubble.js';
import '/js/components/tf-chat-composer.js';
import '/js/components/tf-empty-state.js';
import { TfAgentActivity } from '/js/components/tf-agent-activity.js';
import { activityLabels } from '/js/lib/agent-activity-bridge.js';

// Limits mirror db::repository AGENT_* constants so violations surface inline
// instead of as backend bad_request round-trips.
const NAME_MAX_CHARS = 64;
const MAX_ITERATIONS_CAP = 100;
const KEBAB_REGEX = /^[a-z0-9]+(-[a-z0-9]+)*$/;

// Run status → tf-chip status colour. Mirrors the agent_runs CHECK set.
const RUN_STATUS_CHIP = {
  queued: 'info',
  running: 'accent',
  waiting: 'warn',
  waiting_user: 'warn',
  completed: 'ok',
  failed: 'err',
  cancelled: 'info',
  interrupted: 'warn',
};

const TERMINAL_RUN_STATUSES = ['completed', 'failed', 'cancelled', 'interrupted'];

// Deterministic card avatar icons — a stable icon per agent name keeps the grid
// scannable without storing an icon column.
const CARD_ICONS = ['brain', 'search', 'eye', 'file-text', 'code', 'shield', 'star', 'list', 'globe', 'database', 'zap', 'folder'];

// Wizard step-1 personas: a ready skeleton of description + system prompt.
const PERSONAS = [
  { id: 'researcher', icon: 'search' },
  { id: 'writer', icon: 'file-text' },
  { id: 'critic', icon: 'eye' },
  { id: 'supervisor', icon: 'star' },
  { id: 'tester', icon: 'list' },
  { id: 'custom', icon: 'sparkle' },
];

// Team templates (A05). Prompts reuse the persona presets where the role
// matches; the extra roles carry their own prompt keys. Tools stay empty on
// purpose — installed addons differ per node, so the admin picks them in the
// editor after the team is created.
const TEAM_TEMPLATES = [
  {
    id: 'docqa',
    icon: 'file-text',
    nameKey: 'tpl_docqa_name',
    descKey: 'tpl_docqa_desc',
    loop: true,
    flow: [
      { icon: 'file-text', labelKey: 'persona_writer_name' },
      { icon: 'eye', labelKey: 'persona_critic_name' },
      { icon: 'star', labelKey: 'persona_supervisor_name' },
    ],
    agents: [
      { name: 'dokumentalista', icon: 'file-text', displayKey: 'persona_writer_name', roleKey: 'tpl_agent_writer_role', promptKey: 'persona_writer_prompt' },
      { name: 'krytyk-wymagan', icon: 'eye', displayKey: 'tpl_agent_critic_name', roleKey: 'tpl_agent_critic_role', promptKey: 'persona_critic_prompt' },
      { name: 'nadzorca-procesu', icon: 'star', displayKey: 'tpl_agent_supervisor_name', roleKey: 'tpl_agent_supervisor_role', promptKey: 'persona_supervisor_prompt', maxSubagents: 2, maxSpawnDepth: 2 },
    ],
  },
  {
    id: 'testing',
    icon: 'list',
    nameKey: 'tpl_test_name',
    descKey: 'tpl_test_desc',
    loop: false,
    flow: [
      { icon: 'list', labelKey: 'tpl_agent_generator_name' },
      { icon: 'code', labelKey: 'tpl_agent_executor_name' },
      { icon: 'chart-line', labelKey: 'tpl_agent_reporter_name' },
    ],
    agents: [
      { name: 'generator-scenariuszy', icon: 'list', displayKey: 'tpl_agent_generator_name', roleKey: 'tpl_agent_generator_role', promptKey: 'persona_tester_prompt' },
      { name: 'generator-playwright', icon: 'code', displayKey: 'tpl_agent_executor_name', roleKey: 'tpl_agent_executor_role', promptKey: 'tpl_agent_executor_prompt' },
      { name: 'raporter-wynikow', icon: 'chart-line', displayKey: 'tpl_agent_reporter_name', roleKey: 'tpl_agent_reporter_role', promptKey: 'tpl_agent_reporter_prompt' },
    ],
  },
  {
    id: 'research',
    icon: 'globe',
    nameKey: 'tpl_research_name',
    descKey: 'tpl_research_desc',
    loop: false,
    flow: [
      { icon: 'search', labelKey: 'persona_researcher_name' },
      { icon: 'file-text', labelKey: 'tpl_agent_summarizer_name' },
    ],
    agents: [
      { name: 'researcher-web', icon: 'search', displayKey: 'tpl_agent_researcher_name', roleKey: 'tpl_agent_researcher_role', promptKey: 'persona_researcher_prompt' },
      { name: 'streszczacz', icon: 'file-text', displayKey: 'tpl_agent_summarizer_name', roleKey: 'tpl_agent_summarizer_role', promptKey: 'tpl_agent_summarizer_prompt' },
    ],
  },
];

// core.* builtins grouped semantically for the tools tab (A03): collaboration /
// project knowledge / code & repo / skills. A builtin outside every group
// falls back to "other" so a future core tool never disappears from the UI.
const CORE_TOOL_GROUPS = [
  {
    id: 'collab',
    icon: 'bot',
    titleKey: 'tools_core_collab',
    subKey: 'tools_core_collab_sub',
    names: ['core.agent_spawn', 'core.agent_wait', 'core.agent_list', 'core.agent_cancel', 'core.ask_user'],
  },
  {
    id: 'project',
    icon: 'folder',
    titleKey: 'tools_core_project',
    subKey: 'tools_core_project_sub',
    names: ['core.project_search', 'core.project_list_sources', 'core.case_save', 'core.project_case_save'],
  },
  {
    id: 'code',
    icon: 'code',
    titleKey: 'tools_core_code',
    subKey: 'tools_core_code_sub',
    prefixes: ['core.fs_', 'core.git_', 'core.task_'],
    names: ['core.exec', 'core.code_search', 'core.workspace_info'],
  },
  {
    id: 'skills',
    icon: 'sparkle',
    titleKey: 'tools_core_skills',
    subKey: 'tools_core_skills_sub',
    names: ['core.skill_view'],
  },
];

function coreToolGroupId(name) {
  for (const g of CORE_TOOL_GROUPS) {
    if (g.names?.includes(name)) return g.id;
    if (g.prefixes?.some((p) => name.startsWith(p))) return g.id;
  }
  return 'other';
}

const state = {
  agents: [],
  searchQuery: '',
  enabledFilter: 'all',
  // Mutating flows (upsert/builder-assist) are Admin-only server-side. The role
  // gates whether the create/edit/duplicate/templates/assistant affordances show
  // at all — a non-admin still gets read-only list + detail.
  isAdmin: false,
  // Agent detail (breadcrumb → header → tabs).
  detail: null,
  // Playground session inside the Testowanie tab.
  pg: null,
  wizard: null,
  assist: null,
  templatesWin: null,
  // Top-level Runs tab.
  topTab: 'agents',
  runs: [],
  runsStatusFilter: 'all',
  selectedRunId: null,
  // Live AgentRunEvent steps keyed by run id (in-memory, ephemeral). Reconciled
  // from RunDetail.run_log on open and appended from the run-events stream.
  runSteps: new Map(),
  runsUnsub: null,
  // Monotonic token guarding the run-detail subscription against fast switches:
  // a resolved subscribe handle whose token is stale is torn down immediately.
  runsSubToken: 0,
};

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

// Module windows are tagged so every opener can sweep leftovers of its kind;
// a missed close path must never stack a second copy over the old one.
function sweepWindows(purpose) {
  document.querySelectorAll(`tf-window[data-agents-window="${purpose}"], .tf-window-backdrop[data-agents-window="${purpose}"]`)
    .forEach((el) => el.remove());
}

function t(key, params) {
  return I18n.t(`agents.${key}`, params);
}

const AgentsScreen = {
  get title() { return t('title'); },

  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('brain')} ${escapeHtml(t('title'))}</h1>
          <div class="sub" id="agents-sub"></div>
        </div>
        <div class="actions">
          <tf-button variant="ghost" icon="refresh" id="agents-refresh">${escapeHtml(t('refresh'))}</tf-button>
          <tf-button variant="ghost" icon="users" id="agents-templates">${escapeHtml(t('team_templates'))}</tf-button>
          <tf-button variant="primary" icon="plus" id="agents-new">${escapeHtml(t('new_agent'))}</tf-button>
        </div>
      </div>

      <tf-tabs variant="underline" value="agents" id="agents-top-tabs">
        <tf-tab id="agents" icon="brain">${escapeHtml(t('tab_agents'))}</tf-tab>
        <tf-tab id="runs" icon="clock">${escapeHtml(t('tab_runs'))}</tf-tab>
      </tf-tabs>

      <div class="agents-top-panel" data-top-panel="agents">
        <div id="agents-list-view">
          <div class="agents-toolbar agents-toolbar-cards">
            <tf-searchbox id="agents-search" placeholder="${escapeAttr(t('search_placeholder'))}" debounce="200"></tf-searchbox>
            <tf-filter-chips id="agents-filter-enabled" mode="single"></tf-filter-chips>
          </div>
          <div id="agents-grid-host"></div>
        </div>
        <div id="agents-detail-view" hidden></div>
      </div>

      <div class="agents-top-panel" data-top-panel="runs" hidden>
        <section class="card agents-card">
          <div class="agents-toolbar">
            <tf-select id="runs-filter-status" class="agents-filter" value="all">
              <option value="all">${escapeHtml(t('runs_filter_status_all'))}</option>
              <option value="active">${escapeHtml(t('runs_filter_status_active'))}</option>
              <option value="completed">${escapeHtml(t('run_status_completed'))}</option>
              <option value="failed">${escapeHtml(t('run_status_failed'))}</option>
            </tf-select>
            <tf-button variant="ghost" icon="refresh" id="runs-refresh">${escapeHtml(t('refresh'))}</tf-button>
          </div>
          <div id="runs-table-host" class="agents-runs-host"></div>
          <div id="runs-detail-host" class="agents-run-detail"></div>
        </section>
      </div>
    `;
  },

  async mount() {
    // Role gate uses the real authMeRequest (same pattern as apps-home.js);
    // admin-only actions stay hidden for non-admins instead of failing on save.
    const me = await ApiBinary.one('authMeRequest').catch(() => null);
    state.isAdmin = (me?.role ?? 'user').toLowerCase() === 'admin';
    if (!state.isAdmin) {
      byId('agents-new')?.setAttribute('hidden', '');
      byId('agents-templates')?.setAttribute('hidden', '');
    }

    byId('agents-refresh')?.addEventListener('click', () => {
      if (state.topTab === 'runs') loadRunsTab();
      else loadAgents();
    });
    byId('agents-new')?.addEventListener('click', () => openWizard(null));
    byId('agents-templates')?.addEventListener('click', () => openTemplates());
    byId('agents-search')?.addEventListener('search', (e) => {
      state.searchQuery = String(e.detail?.value ?? '');
      renderGrid();
    });
    const filterChips = byId('agents-filter-enabled');
    if (filterChips) {
      filterChips.filters = [
        { id: 'all', label: t('filter_all'), active: state.enabledFilter === 'all' },
        { id: 'enabled', label: t('filter_enabled'), active: state.enabledFilter === 'enabled' },
        { id: 'disabled', label: t('filter_disabled'), active: state.enabledFilter === 'disabled' },
      ];
      filterChips.addEventListener('change', (e) => {
        state.enabledFilter = e.detail?.id ?? 'all';
        renderGrid();
      });
    }
    byId('agents-top-tabs')?.addEventListener('change', (e) => {
      const id = e.detail?.value;
      if (id) switchTopTab(id);
    });
    byId('runs-refresh')?.addEventListener('click', () => loadRunsTab());
    byId('runs-filter-status')?.addEventListener('change', (e) => {
      state.runsStatusFilter = e.detail?.value ?? e.target.value ?? 'all';
      renderRunsTable();
    });
    wireGridEvents();
    await loadAgents();
  },

  unmount() {
    closeDetail({ keepList: true });
    state.wizard?.cleanup();
    state.assist?.cleanup();
    state.templatesWin?.cleanup();
    // Bump the token so an in-flight run-detail subscribe (whose .then has not
    // resolved yet) closes itself instead of publishing a stale handle.
    state.runsSubToken += 1;
    if (state.runsUnsub) { state.runsUnsub(); state.runsUnsub = null; }
    state.agents = [];
    state.searchQuery = '';
    state.enabledFilter = 'all';
    state.topTab = 'agents';
    state.runs = [];
    state.selectedRunId = null;
    state.runSteps = new Map();
  },
};

export default AgentsScreen;

// =============================================================================
// Data
// =============================================================================

async function loadAgents() {
  try {
    // The list arrives as a JSON string inside AgentsListResponse, not a
    // native array — ApiBinary.list() would find no array key and yield [].
    const resp = await ApiBinary.one('agentsListRequest', {});
    const rows = JSON.parse(resp.agentsJson ?? resp.agents_json ?? '[]');
    state.agents = Array.isArray(rows) ? rows : [];
  } catch (err) {
    toast(`${t('load_failed')}: ${err.message}`, 'error');
    state.agents = [];
  }
  document.querySelector('#agents-top-tabs tf-tab#agents')?.setAttribute('count', String(state.agents.length));
  updateSubLine();
  renderGrid();
}

function updateSubLine() {
  const sub = byId('agents-sub');
  if (!sub) return;
  if (state.detail) {
    sub.textContent = t('detail_open_sub', { name: state.detail.agent.display_name || state.detail.agent.name });
    return;
  }
  const total = state.agents.length;
  const enabled = state.agents.filter((a) => a.is_enabled).length;
  sub.textContent = t('list_sub', { total, enabled });
}

function parseStringArray(json) {
  try {
    const arr = JSON.parse(json || '[]');
    return Array.isArray(arr) ? arr : [];
  } catch {
    return [];
  }
}

function normalizeSkillsSelection(obj) {
  if (Array.isArray(obj)) return { names: obj.map(String), tags: [] };
  if (!obj || typeof obj !== 'object') return { names: [], tags: [] };
  return {
    names: Array.isArray(obj.names) ? obj.names.map(String) : [],
    tags: Array.isArray(obj.tags) ? obj.tags.map(String) : [],
  };
}

function parseSkillsSelection(json) {
  try {
    return normalizeSkillsSelection(JSON.parse(json || '{}'));
  } catch {
    return { names: [], tags: [] };
  }
}

function filteredAgents() {
  const q = state.searchQuery.trim().toLowerCase();
  return state.agents.filter((agent) => {
    if (state.enabledFilter === 'enabled' && !agent.is_enabled) return false;
    if (state.enabledFilter === 'disabled' && agent.is_enabled) return false;
    if (!q) return true;
    const hay = `${agent.display_name || ''} ${agent.name} ${agent.description || ''}`.toLowerCase();
    return hay.includes(q);
  });
}

function countSkills(agent) {
  const sel = typeof agent.skills === 'object' && agent.skills !== null && !Array.isArray(agent.skills)
    ? normalizeSkillsSelection(agent.skills)
    : parseSkillsSelection(typeof agent.skills === 'string' ? agent.skills : '{}');
  return sel.names.length + sel.tags.length;
}

function agentIcon(name) {
  let hash = 0;
  for (let i = 0; i < String(name).length; i++) hash = (hash * 31 + String(name).charCodeAt(i)) >>> 0;
  return CARD_ICONS[hash % CARD_ICONS.length];
}

function formatTimestamp(value) {
  if (!value) return '—';
  const d = new Date(String(value).includes('T') ? value : String(value).replace(' ', 'T'));
  if (Number.isNaN(d.getTime())) return String(value);
  return d.toLocaleString(I18n.getLanguage(), { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' });
}

// =============================================================================
// List (A01) — cards
// =============================================================================

function renderGrid() {
  const host = byId('agents-grid-host');
  if (!host) return;
  const visible = filteredAgents();

  if (!visible.length && state.agents.length) {
    host.innerHTML = `<tf-empty-state icon="brain" title="${escapeAttr(t('empty_match'))}"></tf-empty-state>`;
    return;
  }

  const cards = visible.map(agentCardHtml).join('');
  const addCard = state.isAdmin ? `
    <div class="agent-card agent-card-add" data-add-card role="button" tabindex="0">
      <div class="agent-add-ico">${sprite('plus')}</div>
      <div class="agent-add-name">${escapeHtml(t('new_agent'))}</div>
      <div class="agent-add-sub">${escapeHtml(t('add_card_sub'))}</div>
    </div>
  ` : '';
  host.innerHTML = `<div class="agents-grid">${cards}${addCard}</div>`;
}

function agentCardHtml(agent) {
  const toolsCount = parseStringArray(agent.tools_json).length;
  const skillsCount = countSkills(agent);
  const enableItem = agent.is_enabled
    ? `<tf-menu-item action="disable" icon="ban">${escapeHtml(t('action_disable'))}</tf-menu-item>`
    : `<tf-menu-item action="enable" icon="check">${escapeHtml(t('action_enable'))}</tf-menu-item>`;
  // Non-admins get only the read-only "Try out" action; every other item calls
  // an Admin-only upsert/delete, so the whole menu collapses to one entry.
  const menu = state.isAdmin
    ? `<div class="agent-card-menu-wrap">
        <tf-button variant="ghost" size="sm" icon="chevron-down" data-more title="${escapeAttr(t('action_more'))}"></tf-button>
        <tf-menu placement="bottom-end" data-card-menu>
          <tf-menu-item action="edit" icon="edit">${escapeHtml(t('action_edit'))}</tf-menu-item>
          <tf-menu-item action="duplicate" icon="copy">${escapeHtml(t('action_duplicate'))}</tf-menu-item>
          <tf-menu-item action="try" icon="play">${escapeHtml(t('action_try'))}</tf-menu-item>
          ${enableItem}
          <tf-menu-divider></tf-menu-divider>
          <tf-menu-item action="delete" icon="trash" danger>${escapeHtml(t('action_delete'))}</tf-menu-item>
        </tf-menu>
      </div>`
    : `<div class="agent-card-menu-wrap">
        <tf-button variant="ghost" size="sm" icon="play" data-try title="${escapeAttr(t('action_try'))}"></tf-button>
      </div>`;
  return `
    <div class="agent-card ${agent.is_enabled ? '' : 'is-disabled'}" data-agent-id="${escapeAttr(agent.id)}" role="button" tabindex="0">
      <div class="agent-card-top">
        <div class="agent-card-av">${sprite(agentIcon(agent.name))}</div>
        <div class="agent-card-heading">
          <div class="agent-card-name">${escapeHtml(agent.display_name || agent.name)}</div>
          <div class="agent-card-role">${escapeHtml(agent.name)} · ${escapeHtml(agent.model || t('model_inherited'))}</div>
        </div>
      </div>
      <div class="agent-card-desc">${escapeHtml(agent.description || '')}</div>
      <div class="agent-card-meta">
        <span>${sprite('bolt')}${escapeHtml(t('card_tools_count', { count: toolsCount }))}</span>
        <span>${sprite('sparkle')}${escapeHtml(t('card_skills_count', { count: skillsCount }))}</span>
      </div>
      <div class="agent-card-foot">
        ${agent.routable ? `<tf-chip status="info">${escapeHtml(t('chip_routable'))}</tf-chip>` : ''}
        <tf-chip status="${agent.is_enabled ? 'ok' : 'warn'}" dot>${escapeHtml(t(agent.is_enabled ? 'enabled_yes' : 'enabled_no'))}</tf-chip>
        ${menu}
      </div>
    </div>
  `;
}

// One delegated listener survives every grid re-render; per-card menus stay
// statically declared (tf-menu requirement) inside the card HTML.
function wireGridEvents() {
  const host = byId('agents-grid-host');
  if (!host) return;

  host.addEventListener('click', (e) => {
    const more = e.target.closest('[data-more]');
    if (more) {
      e.stopPropagation();
      more.parentElement?.querySelector('[data-card-menu]')?.toggle();
      return;
    }
    const tryBtn = e.target.closest('[data-try]');
    if (tryBtn) {
      e.stopPropagation();
      const tryCard = tryBtn.closest('[data-agent-id]');
      if (tryCard) openDetail(tryCard.dataset.agentId, 'test');
      return;
    }
    if (e.target.closest('[data-card-menu]')) return;
    const addCard = e.target.closest('[data-add-card]');
    if (addCard) {
      openWizard(null);
      return;
    }
    const card = e.target.closest('[data-agent-id]');
    if (card) openDetail(card.dataset.agentId, 'config');
  });

  host.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    const addCard = e.target.closest('[data-add-card]');
    if (addCard) {
      e.preventDefault();
      openWizard(null);
      return;
    }
    const card = e.target.closest('[data-agent-id]');
    if (card && e.target === card) {
      e.preventDefault();
      openDetail(card.dataset.agentId, 'config');
    }
  });

  // Menu items are declarative; resolve actions here so each re-render keeps a
  // single listener instead of rebinding per menu.
  host.addEventListener('tf-menu-action', (e) => {
    const action = e.detail?.action;
    const card = e.target.closest('[data-agent-id]');
    if (!card) return;
    const id = card.dataset.agentId;
    if (action === 'edit') openDetail(id, 'config');
    else if (action === 'duplicate') duplicateAgent(id);
    else if (action === 'try') openDetail(id, 'test');
    else if (action === 'enable' || action === 'disable') setAgentEnabled(id, action === 'enable');
    else if (action === 'delete') deleteAgent(id);
  }, true);
}

async function fetchAgentDetail(agentId) {
  const resp = await ApiBinary.one('agentsDetailRequest', { agentId });
  return JSON.parse(resp.agentJson ?? resp.agent_json ?? 'null');
}

// A full upsert payload from a detail row — enable/disable and duplicate flows
// must round-trip every field, not just the one they change. The detail row is
// already the upsert shape, so this only applies the overrides.
function payloadFromDetail(agent, overrides = {}) {
  return {
    id: agent.id,
    name: agent.name,
    display_name: agent.display_name || null,
    description: agent.description || '',
    system_prompt: agent.system_prompt || null,
    model: agent.model || null,
    tools: agent.tools ?? [],
    skills: normalizeSkillsSelection(agent.skills),
    params: agent.params ?? {},
    max_iterations: agent.max_iterations ?? 25,
    timeout_secs: agent.timeout_secs ?? 600,
    max_subagents: agent.max_subagents ?? 0,
    max_spawn_depth: agent.max_spawn_depth ?? 1,
    on_child_complete: agent.on_child_complete || 'notify',
    flow_id: agent.flow_id || null,
    routable: !!agent.routable,
    is_enabled: !!agent.is_enabled,
    ...overrides,
  };
}

async function setAgentEnabled(agentId, enabled) {
  try {
    const agent = await fetchAgentDetail(agentId);
    if (!agent) return;
    await ApiBinary.one('agentsUpsertRequest', {
      agentJson: JSON.stringify(payloadFromDetail(agent, { is_enabled: enabled })),
    });
    toast(t('save_ok'), 'success');
    await loadAgents();
    if (state.detail?.agent.id === agentId) {
      state.detail.agent.is_enabled = enabled;
      renderDetailHeader();
    }
  } catch (err) {
    toast(`${t('save_failed')}: ${err.message}`, 'error');
  }
}

function uniqueCopyName(baseName) {
  const existing = new Set(state.agents.map((a) => a.name));
  let candidate = `${baseName}-copy`.slice(0, NAME_MAX_CHARS);
  let n = 2;
  while (existing.has(candidate)) {
    const suffix = `-copy-${n}`;
    candidate = `${baseName.slice(0, NAME_MAX_CHARS - suffix.length)}${suffix}`;
    n += 1;
  }
  return candidate;
}

// Duplicate opens the CREATE wizard pre-filled from the source row — the copy
// does not exist yet, so the wizard (not inline editing) is the right surface.
async function duplicateAgent(agentId) {
  try {
    const agent = await fetchAgentDetail(agentId);
    if (!agent) return;
    const copy = { ...agent, id: null, name: uniqueCopyName(agent.name) };
    openWizard(copy, { forceCreate: true });
  } catch (err) {
    toast(`${t('load_failed')}: ${err.message}`, 'error');
  }
}

async function deleteAgent(agentId) {
  const agent = state.agents.find((a) => a.id === agentId);
  if (!agent) return;
  const ok = await TfWindow.confirm({
    title: t('delete_confirm_title'),
    message: t('delete_confirm_message', { name: escapeHtml(agent.name) }),
    confirmLabel: t('action_delete'),
    cancelLabel: t('action_cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await ApiBinary.one('agentsDeleteRequest', { agentId });
    toast(t('delete_ok'), 'success');
    if (state.detail?.agent.id === agentId) closeDetail();
    await loadAgents();
  } catch (err) {
    toast(`${t('delete_failed')}: ${err.message}`, 'error');
  }
}

// =============================================================================
// Detail shell (A02–A05) — breadcrumb → header → tabs → tab body
// =============================================================================

const DETAIL_TABS = [
  { id: 'config', icon: 'settings', labelKey: 'tab_config' },
  { id: 'tools', icon: 'bolt', labelKey: 'tab_tools' },
  { id: 'test', icon: 'play', labelKey: 'tab_test' },
  { id: 'runs', icon: 'clock', labelKey: 'tab_runs_agent' },
];

async function openDetail(agentId, tab = 'config') {
  let agent = null;
  try {
    agent = await fetchAgentDetail(agentId);
  } catch (err) {
    toast(`${t('detail_failed')}: ${err.message}`, 'error');
    return;
  }
  if (!agent) return;

  closeDetail({ keepList: true });

  const skills = normalizeSkillsSelection(agent.skills);
  const params = agent.params && typeof agent.params === 'object' ? agent.params : {};
  // Editable draft: every field the config tab renders. `sourceParams` carries
  // params_json keys this UI does not manage so a save never drops them.
  const cfg = {
    name: agent.name || '',
    display_name: agent.display_name || '',
    description: agent.description || '',
    system_prompt: agent.system_prompt || '',
    model: agent.model || '',
    temperature: params.temperature ?? '',
    reasoning_effort: params.reasoning_effort ?? '',
    max_iterations: agent.max_iterations ?? 25,
    timeout_secs: agent.timeout_secs ?? 600,
    max_subagents: agent.max_subagents ?? 0,
    max_spawn_depth: agent.max_spawn_depth ?? 1,
    on_child_complete: agent.on_child_complete || 'notify',
    flow_id: agent.flow_id || '',
    routable: !!agent.routable,
    is_enabled: !!agent.is_enabled,
  };

  state.detail = {
    agentId,
    agent,
    tab,
    cfg,
    sourceParams: { ...params },
    dirty: false,
    catalog: { addons: [], core: [] },
    skills: [],
    models: [],
    selectedTools: new Set(Array.isArray(agent.tools) ? agent.tools : []),
    skillNames: new Set(skills.names),
    skillTags: new Set(skills.tags),
    toolsSearch: '',
    pickersLoaded: false,
    runsLoaded: false,
    agentRuns: [],
    agentRunsFilter: 'all',
    selectedAgentRunId: null,
    agentRunSteps: new Map(),
    agentRunsUnsub: null,
    agentRunsToken: 0,
  };

  const listView = byId('agents-list-view');
  const detailView = byId('agents-detail-view');
  if (listView) listView.hidden = true;
  if (!detailView) return;
  detailView.hidden = false;
  detailView.innerHTML = `
    <div class="ag-detail">
      <div class="breadcrumb ag-breadcrumb">
        <span class="crumb" data-crumb-root>${escapeHtml(t('title'))}</span>
        <span class="sep">${sprite('chevron-right')}</span>
        <span class="crumb active" data-crumb-current></span>
      </div>
      <div id="ag-detail-header-host"></div>
      <div id="ag-detail-tabs-host"></div>
      <div id="ag-detail-body"></div>
    </div>
  `;
  detailView.querySelector('[data-crumb-root]')?.addEventListener('click', () => closeDetail());
  renderDetailHeader();
  renderDetailTabs();
  // Picker/model data feeds every tab; loading it up front keeps the first
  // paint of each tab complete instead of popping options in afterwards.
  const [catalogRows, skillRows, modelRows] = await Promise.all([loadToolsCatalog(), loadSkillList(), loadModelOptions()]);
  const dd = state.detail;
  if (!dd || dd.agentId !== agentId) return;
  dd.catalog = catalogRows;
  dd.skills = skillRows;
  dd.models = modelRows;
  await switchDetailTab(tab);
  updateSubLine();
}

function closeDetail({ keepList = false } = {}) {
  teardownPg();
  const d = state.detail;
  if (d) {
    d.agentRunsToken += 1;
    if (d.agentRunsUnsub) { d.agentRunsUnsub(); d.agentRunsUnsub = null; }
  }
  state.detail = null;
  if (keepList) return;
  const listView = byId('agents-list-view');
  const detailView = byId('agents-detail-view');
  if (detailView) { detailView.hidden = true; detailView.innerHTML = ''; }
  if (listView) listView.hidden = false;
  loadAgents();
  updateSubLine();
}

// Re-reads the durable row after a save/enable flip and refreshes the header
// badges without collapsing the tab the operator is working in.
async function refreshDetailAgent() {
  const d = state.detail;
  if (!d) return;
  const fresh = await fetchAgentDetail(d.agentId).catch(() => null);
  if (!fresh || state.detail !== d) return;
  d.agent = fresh;
  renderDetailHeader();
  renderDetailTabs();
}

function renderDetailHeader() {
  const d = state.detail;
  const host = byId('ag-detail-header-host');
  if (!d || !host) return;
  const agent = d.agent;
  const toolsCount = d.selectedTools.size;
  const skillsCount = d.skillNames.size + d.skillTags.size;
  const badges = [
    `<tf-chip status="accent">${escapeHtml(t('label_model'))}: ${escapeHtml(agent.model || t('model_inherited'))}</tf-chip>`,
    `<tf-chip>${escapeHtml(t('card_tools_count', { count: toolsCount }))}</tf-chip>`,
    `<tf-chip>${escapeHtml(t('card_skills_count', { count: skillsCount }))}</tf-chip>`,
    `<tf-chip>${escapeHtml(t('hdr_loop_limits', { iterations: agent.max_iterations ?? 25, timeout: agent.timeout_secs ?? 600 }))}</tf-chip>`,
  ];
  if ((agent.max_subagents ?? 0) > 0) {
    badges.push(`<tf-chip>${escapeHtml(t('hdr_subagents', { count: agent.max_subagents }))}</tf-chip>`);
  }
  const adminActions = state.isAdmin ? `
    <tf-button variant="primary" icon="play" data-hdr-test>${escapeHtml(t('hdr_run_test'))}</tf-button>
    <tf-button variant="ghost" icon="copy" data-hdr-duplicate>${escapeHtml(t('action_duplicate'))}</tf-button>
    <tf-button variant="ghost" icon="${agent.is_enabled ? 'ban' : 'check'}" data-hdr-toggle>${escapeHtml(t(agent.is_enabled ? 'disable_agent' : 'enable_agent'))}</tf-button>
    <tf-button variant="danger" icon="trash" data-hdr-delete>${escapeHtml(t('action_delete'))}</tf-button>
  ` : '';
  host.innerHTML = `
    <div class="detail-header ag-detail-header">
      <div class="big-ico">${sprite(agentIcon(agent.name))}</div>
      <div class="d-meta">
        <div class="d-name">
          ${escapeHtml(agent.display_name || agent.name)}
          <tf-chip status="${agent.is_enabled ? 'ok' : 'warn'}" dot>${escapeHtml(t(agent.is_enabled ? 'enabled_yes' : 'enabled_no'))}</tf-chip>
          ${agent.routable ? `<tf-chip status="info">${escapeHtml(t('chip_routable'))}</tf-chip>` : ''}
        </div>
        <div class="d-sub mono">${escapeHtml(agent.name)}</div>
        <div class="d-badges">${badges.join('')}</div>
      </div>
      <div class="d-actions">${adminActions}</div>
    </div>
  `;
  // The breadcrumb lives outside the header host — update it via the document.
  const crumbCurrent = document.querySelector('.ag-breadcrumb [data-crumb-current]');
  if (crumbCurrent) crumbCurrent.textContent = agent.display_name || agent.name;
  host.querySelector('[data-hdr-test]')?.addEventListener('click', () => switchDetailTab('test'));
  host.querySelector('[data-hdr-duplicate]')?.addEventListener('click', () => duplicateAgent(d.agentId));
  host.querySelector('[data-hdr-toggle]')?.addEventListener('click', () => setAgentEnabled(d.agentId, !d.agent.is_enabled));
  host.querySelector('[data-hdr-delete]')?.addEventListener('click', () => deleteAgent(d.agentId));
}

function renderDetailTabs() {
  const d = state.detail;
  const host = byId('ag-detail-tabs-host');
  if (!d || !host) return;
  const counts = {
    tools: String(d.selectedTools.size),
    runs: d.agent.run_count_30d != null ? String(d.agent.run_count_30d) : null,
  };
  host.innerHTML = `
    <tf-tabs variant="underline" value="${escapeAttr(d.tab)}" id="agent-detail-tabs">
      ${DETAIL_TABS.map((tab) => `
        <tf-tab id="${escapeAttr(tab.id)}" icon="${escapeAttr(tab.icon)}"${counts[tab.id] ? ` count="${escapeAttr(counts[tab.id])}"` : ''}>${escapeHtml(t(tab.labelKey))}</tf-tab>
      `).join('')}
    </tf-tabs>
  `;
  host.querySelector('#agent-detail-tabs')?.addEventListener('change', (e) => {
    const id = e.detail?.value;
    if (id) switchDetailTab(id);
  });
}

// Updates the Tools tab badge without re-rendering the tab strip itself —
// this runs inside a change event bubbling out of that very strip.
function updateDetailTabCounts() {
  const d = state.detail;
  const tab = document.querySelector('#agent-detail-tabs tf-tab#tools');
  if (d && tab) tab.setAttribute('count', String(d.selectedTools.size));
}

async function switchDetailTab(tab) {
  const d = state.detail;
  if (!d) return;
  d.tab = DETAIL_TABS.some((x) => x.id === tab) ? tab : 'config';
  const tabsNav = byId('ag-detail-tabs-host')?.querySelector('#agent-detail-tabs');
  if (tabsNav) tabsNav.setAttribute('value', d.tab);
  const body = byId('ag-detail-body');
  if (!body) return;
  body.innerHTML = '';

  if (d.tab === 'config') {
    renderConfigTab(body);
    return;
  }
  if (d.tab === 'tools') {
    await renderToolsTab(body);
    return;
  }
  if (d.tab === 'test') {
    renderTestTab(body);
    return;
  }
  if (d.tab === 'runs') {
    await renderAgentRunsTab(body);
    return;
  }
}

// =============================================================================
// Dirty tracking + sticky save bar (Konfiguracja / Narzędzia share one draft)
// =============================================================================

function markDirty() {
  const d = state.detail;
  if (!d) return;
  d.dirty = true;
  updateSaveBar();
}

function discardDraftChanges() {
  const d = state.detail;
  if (!d) return;
  openDetail(d.agentId, d.tab);
}

async function saveDetailDraft() {
  const d = state.detail;
  if (!d) return;
  const error = validateDraft();
  if (error) {
    toast(error, 'error');
    return;
  }
  const c = d.cfg;
  // params_json is a shared bag: keys this UI does not manage survive a save
  // untouched; an empty temperature/reasoning field REMOVES the key rather than
  // storing an empty string (the backend would reject "" as a reasoning level).
  const params = { ...d.sourceParams };
  if (c.temperature === '' || c.temperature == null) delete params.temperature;
  else params.temperature = Number(c.temperature);
  if (!c.reasoning_effort) delete params.reasoning_effort;
  else params.reasoning_effort = c.reasoning_effort;

  const payload = {
    id: d.agent.id,
    name: c.name.trim(),
    display_name: c.display_name.trim() || null,
    description: c.description.trim(),
    system_prompt: c.system_prompt.trim() || null,
    model: c.model.trim() || null,
    tools: [...d.selectedTools],
    skills: { names: [...d.skillNames], tags: [...d.skillTags] },
    params,
    max_iterations: Number.parseInt(c.max_iterations, 10) || 25,
    timeout_secs: Number.parseInt(c.timeout_secs, 10) || 600,
    max_subagents: Number.parseInt(c.max_subagents, 10) || 0,
    max_spawn_depth: Number.parseInt(c.max_spawn_depth, 10) || 1,
    on_child_complete: c.on_child_complete || 'notify',
    flow_id: c.flow_id.trim() || null,
    routable: !!c.routable,
    is_enabled: !!c.is_enabled,
  };
  try {
    await ApiBinary.one('agentsUpsertRequest', { agentJson: JSON.stringify(payload) });
    toast(t('save_ok'), 'success');
    d.dirty = false;
    updateSaveBar();
    await refreshDetailAgent();
    await loadAgents();
  } catch (err) {
    toast(`${t('save_failed')}: ${err.message}`, 'error');
  }
}

function validateDraft() {
  const c = state.detail?.cfg;
  if (!c) return null;
  const name = c.name.trim();
  if (!name) return t('err_name_required');
  if (name.length > NAME_MAX_CHARS) return t('err_name_length');
  if (!KEBAB_REGEX.test(name)) return t('err_name_format');
  const takenByOther = state.agents.some((a) => a.name === name && a.id !== state.detail.agentId);
  if (takenByOther) return t('err_name_taken');
  const temp = Number(c.temperature);
  if (c.temperature !== '' && c.temperature != null && (!Number.isFinite(temp) || temp < 0 || temp > 2)) {
    return t('err_temperature');
  }
  return null;
}

function renderSaveBar(host) {
  const d = state.detail;
  if (!d || !host) return;
  if (!d.dirty) return;
  const bar = document.createElement('div');
  bar.className = 'ag-save-bar';
  bar.innerHTML = `
    <span class="ag-save-bar-text">${sprite('alert')} ${escapeHtml(t('save_bar_dirty'))}</span>
    <div class="ag-save-bar-actions">
      <tf-button variant="ghost" data-draft-discard>${escapeHtml(t('action_discard'))}</tf-button>
      <tf-button variant="primary" icon="save" data-draft-save>${escapeHtml(t('action_save'))}</tf-button>
    </div>
  `;
  bar.querySelector('[data-draft-save]')?.addEventListener('click', saveDetailDraft);
  bar.querySelector('[data-draft-discard]')?.addEventListener('click', discardDraftChanges);
  host.appendChild(bar);
}

function updateSaveBar() {
  const d = state.detail;
  if (!d) return;
  const host = byId('ag-detail-body');
  if (!host) return;
  const existing = host.querySelector('.ag-save-bar');
  if (d.dirty && !existing) renderSaveBar(host);
  else if (!d.dirty && existing) existing.remove();
}

// =============================================================================
// Tab: Konfiguracja (A02) — inline editable form sections
// =============================================================================

async function loadModelOptions() {
  const [modelsRaw, aliasesRaw] = await Promise.all([
    ApiBinary.list('modelListRequest', { arrayKey: 'models' }).catch(() => []),
    ApiBinary.list('modelAliasListRequest', { arrayKey: 'aliases' }).catch(() => []),
  ]);
  const models = Array.isArray(modelsRaw) ? modelsRaw : [];
  const aliases = Array.isArray(aliasesRaw) ? aliasesRaw : [];
  const modelByName = new Map();
  for (const m of models) {
    const name = m.model_name || m.modelName;
    if (name) modelByName.set(name, m);
  }
  const opts = models
    .filter((m) => (m.category || '').toLowerCase() === 'llm')
    .map((m) => {
      const value = m.model_name || m.modelName || '';
      const display = m.display_name || m.displayName || value;
      const engine = m.engine_id || m.engineId;
      return { value, label: engine ? `${display} (${engine})` : display };
    })
    .filter((o) => o.value);
  for (const a of aliases) {
    if (a.is_active === false || a.isActive === false) continue;
    const target = a.target_model || a.targetModel;
    const targetModel = target ? modelByName.get(target) : null;
    if (!targetModel) continue;
    if ((targetModel.category || '').toLowerCase() !== 'llm') continue;
    opts.push({ value: a.alias, label: `↪ ${a.alias} → ${target}` });
  }
  return opts;
}

/// Wypełnia listę poziomów rozumowania z katalogu dla AKTUALNIE wybranego modelu.
///
/// Trzy stany, celowo rozróżnione:
///  - model deklaruje poziomy → pokazujemy dokładnie je,
///  - model deklaruje pustą listę → chowamy pole, bo backend odrzuciłby każdą
///    wartość,
///  - katalog nie zna modelu (`null`) → zostawiamy zapisany wybór i pokazujemy
///    pole, tak jak przy modalnościach: brak wpisu to niewiedza, nie brak
///    możliwości.
async function refreshReasoningOptionsFor(select, fieldHost) {
  if (!select || !fieldHost) return;
  await ModelModalities.load();
  const levels = ModelModalities.reasoningLevels(select.dataset.cfgModelValue || '');
  const current = select.value || '';
  if (Array.isArray(levels) && levels.length === 0) {
    fieldHost.hidden = true;
    select.value = '';
    return;
  }
  const options = Array.isArray(levels) && levels.length ? levels : (current ? [current] : []);
  if (!options.length) {
    fieldHost.hidden = true;
    select.value = '';
    return;
  }
  fieldHost.hidden = false;
  select.innerHTML = `<option value="">${escapeHtml(t('reasoning_effort_default'))}</option>`
    + options.map((lvl) => `<option value="${escapeAttr(lvl)}">${escapeHtml(lvl)}</option>`).join('');
  select.value = options.includes(current) ? current : '';
}

function sectionCard(icon, titleKey, inner) {
  return `
    <section class="card agents-section">
      <h3 class="agents-section-title">${sprite(icon)} ${escapeHtml(t(titleKey))}</h3>
      ${inner}
    </section>
  `;
}

function renderConfigTab(body) {
  const d = state.detail;
  if (!d || !body) return;
  const c = d.cfg;
  const iterations = Math.min(Math.max(Number(c.max_iterations) || 25, 1), MAX_ITERATIONS_CAP);

  body.innerHTML = `
    ${sectionCard('bot', 'section_identity', `
      <div class="agents-editor-grid">
        <tf-input data-cfg="name" label="${escapeAttr(t('label_name'))}"
          value="${escapeAttr(c.name)}"
          hint="${escapeAttr(t('hint_name'))}"
          maxlength="${NAME_MAX_CHARS}"></tf-input>
        <tf-input data-cfg="display_name" label="${escapeAttr(t('label_display_name'))}"
          value="${escapeAttr(c.display_name)}"></tf-input>
      </div>
      <tf-textarea data-cfg="description" label="${escapeAttr(t('label_description'))}"
        rows="2" hint="${escapeAttr(t('hint_description'))}"
        value="${escapeAttr(c.description)}"></tf-textarea>
    `)}

    ${sectionCard('terminal', 'section_prompt', `
      <tf-textarea data-cfg="system_prompt" class="mono" rows="9" hint="${escapeAttr(t('hint_system_prompt'))}"
        value="${escapeAttr(c.system_prompt)}"></tf-textarea>
      <div class="ag-prompt-foot">
        <span class="ag-prompt-count" data-prompt-count></span>
        ${state.isAdmin ? `
          <tf-button variant="ghost" size="sm" icon="sparkle" data-prompt-assist>${escapeHtml(t('prompt_improve_ai'))}</tf-button>
          <tf-button variant="ghost" size="sm" icon="refresh" data-prompt-restore>${escapeHtml(t('prompt_restore'))}</tf-button>
        ` : ''}
      </div>
    `)}

    ${sectionCard('brain', 'section_model', `
      <div class="agents-editor-grid">
        <div>
          <tf-select data-cfg="model" label="${escapeAttr(t('label_model'))}">
            <option value="">${escapeHtml(t('model_inherited'))}</option>
            ${d.models.map((m) => `<option value="${escapeAttr(m.value)}" ${m.value === c.model ? 'selected' : ''}>${escapeHtml(m.label)}</option>`).join('')}
          </tf-select>
          <div class="agents-field-hint">${escapeHtml(t('model_hint'))}</div>
        </div>
        <div>
          <tf-input data-cfg="temperature" type="number" min="0" max="2" step="0.1"
            label="${escapeAttr(t('label_temperature'))}"
            value="${escapeAttr(c.temperature ?? '')}"></tf-input>
          <div class="agents-field-hint">${escapeHtml(t('hint_temperature'))}</div>
        </div>
        <div data-reasoning-field hidden>
          <tf-select data-cfg="reasoning_effort" label="${escapeAttr(t('label_reasoning_effort'))}"></tf-select>
          <div class="agents-field-hint">${escapeHtml(t('hint_reasoning_effort'))}</div>
        </div>
      </div>
    `)}

    ${sectionCard('clock', 'section_loop', `
      <div class="agents-slider-row">
        <div>
          <div class="agents-slider-name">${escapeHtml(t('label_max_iterations'))}</div>
          <div class="agents-field-hint">${escapeHtml(t('slider_iterations_desc'))}</div>
        </div>
        <tf-slider data-cfg-slider="max_iterations" min="1" max="${MAX_ITERATIONS_CAP}" step="1"
          value="${escapeAttr(String(iterations))}"></tf-slider>
        <div class="agents-slider-val" data-iterations-val>${escapeHtml(String(iterations))}</div>
      </div>
      <div class="agents-editor-grid agents-limits">
        <tf-input data-cfg="timeout_secs" type="number" label="${escapeAttr(t('label_timeout_secs'))}"
          value="${escapeAttr(String(c.timeout_secs ?? 600))}" min="1"></tf-input>
        <tf-input data-cfg="max_subagents" type="number" label="${escapeAttr(t('label_max_subagents'))}"
          value="${escapeAttr(String(c.max_subagents ?? 0))}" min="0"></tf-input>
        <tf-input data-cfg="max_spawn_depth" type="number" label="${escapeAttr(t('label_max_spawn_depth'))}"
          value="${escapeAttr(String(c.max_spawn_depth ?? 1))}" min="1"></tf-input>
        <div>
          <tf-select data-cfg="on_child_complete" label="${escapeAttr(t('label_on_child_complete'))}">
            <option value="notify" ${c.on_child_complete !== 'continue' ? 'selected' : ''}>${escapeHtml(t('on_child_complete_notify'))}</option>
            <option value="continue" ${c.on_child_complete === 'continue' ? 'selected' : ''}>${escapeHtml(t('on_child_complete_continue'))}</option>
          </tf-select>
          <div class="agents-field-hint">${escapeHtml(t('on_child_complete_hint'))}</div>
        </div>
      </div>
    `)}

    ${sectionCard('settings', 'section_behavior', `
      <div class="agents-toggle-card">
        <tf-toggle data-cfg-toggle="routable" ${c.routable ? 'checked' : ''}></tf-toggle>
        <div>
          <div class="agents-toggle-name">${escapeHtml(t('label_routable'))}</div>
          <div class="agents-field-hint">${escapeHtml(t('routable_desc'))}</div>
        </div>
      </div>
      <div class="agents-toggle-card">
        <tf-toggle data-cfg-toggle="is_enabled" ${c.is_enabled ? 'checked' : ''}></tf-toggle>
        <div>
          <div class="agents-toggle-name">${escapeHtml(t('label_enabled'))}</div>
          <div class="agents-field-hint">${escapeHtml(t('enabled_desc'))}</div>
        </div>
      </div>
      <tf-input data-cfg="flow_id" label="${escapeAttr(t('label_flow_id'))}"
        value="${escapeAttr(c.flow_id || '')}"
        hint="${escapeAttr(t('hint_flow_id'))}"></tf-input>
    `)}
  `;

  wireConfigInputs(body);
  updatePromptCount();
  refreshReasoningOptionsFor(
    body.querySelector('[data-cfg="reasoning_effort"]'),
    body.querySelector('[data-reasoning-field]'),
  );
}

function wireConfigInputs(body) {
  const d = state.detail;
  if (!d) return;
  const readField = (el) => {
    const key = el.getAttribute('data-cfg') || el.getAttribute('data-cfg-slider') || el.getAttribute('data-cfg-toggle');
    if (!key) return;
    if (el.hasAttribute('data-cfg-toggle')) {
      d.cfg[key] = el.hasAttribute('checked');
    } else if (el.tagName === 'TF-SLIDER') {
      d.cfg[key] = el.value;
      const val = body.querySelector('[data-iterations-val]');
      if (val && key === 'max_iterations') val.textContent = String(el.value);
    } else {
      d.cfg[key] = el.value ?? '';
    }
    markDirty();
  };
  body.querySelectorAll('[data-cfg], [data-cfg-toggle], [data-cfg-slider]').forEach((el) => {
    el.addEventListener(el.hasAttribute('data-cfg-slider') ? 'input' : 'change', () => readField(el));
    if (el.tagName === 'TF-INPUT' || el.tagName === 'TF-TEXTAREA') {
      el.addEventListener('input', () => {
        const key = el.getAttribute('data-cfg');
        if (!key) return;
        d.cfg[key] = el.value ?? '';
        if (key === 'system_prompt') updatePromptCount();
        markDirty();
      });
    }
  });

  const modelSelect = body.querySelector('[data-cfg="model"]');
  modelSelect?.addEventListener('change', () => {
    const reasoning = body.querySelector('[data-cfg="reasoning_effort"]');
    if (reasoning) reasoning.dataset.cfgModelValue = modelSelect.value || '';
    refreshReasoningOptionsFor(reasoning, body.querySelector('[data-reasoning-field]'));
  });
  if (modelSelect) {
    const reasoning = body.querySelector('[data-cfg="reasoning_effort"]');
    if (reasoning) reasoning.dataset.cfgModelValue = modelSelect.value || '';
  }

  body.querySelector('[data-prompt-assist]')?.addEventListener('click', () => openAssist('detail'));
  body.querySelector('[data-prompt-restore]')?.addEventListener('click', () => {
    const dd = state.detail;
    if (!dd) return;
    // Restore reverts ONLY the draft textarea to the last saved value; other
    // dirty edits stay untouched.
    dd.cfg.system_prompt = dd.agent.system_prompt || '';
    const ta = body.querySelector('tf-textarea[data-cfg="system_prompt"]');
    if (ta) ta.setAttribute('value', dd.cfg.system_prompt);
    updatePromptCount();
    markDirty();
    toast(t('prompt_restored'), 'success');
  });
}

function updatePromptCount() {
  const el = document.querySelector('#ag-detail-body [data-prompt-count]');
  if (!el) return;
  const prompt = state.detail?.cfg.system_prompt || '';
  el.textContent = prompt
    ? t('prompt_chars', { chars: prompt.length })
    : t('prompt_empty_hint');
}

// =============================================================================
// Tab: Narzędzia i umiejętności (A03)
// =============================================================================

async function loadToolsCatalog() {
  try {
    const resp = await ApiBinary.one('toolsCatalogRequest', {});
    const parsed = JSON.parse(resp.toolsJson ?? resp.tools_json ?? '{}');
    return {
      addons: Array.isArray(parsed?.addons) ? parsed.addons : [],
      core: Array.isArray(parsed?.core) ? parsed.core : [],
    };
  } catch {
    return { addons: [], core: [] };
  }
}

async function loadSkillList() {
  try {
    const resp = await ApiBinary.one('skillsListRequest', {});
    const rows = JSON.parse(resp.skillsJson ?? resp.skills_json ?? '[]');
    return Array.isArray(rows) ? rows : [];
  } catch {
    return [];
  }
}

async function renderToolsTab(body) {
  const d = state.detail;
  if (!d || !body) return;
  if (!d.pickersLoaded) {
    // Drop allowlist entries that no longer exist anywhere — a stale wildcard or
    // an uninstalled-and-renamed addon would silently admit nothing.
    const known = new Set(d.catalog.core.map((tool) => tool.name));
    for (const group of d.catalog.addons) {
      known.add(`${group.addon_id}.*`);
      for (const tool of group.tools) known.add(tool.name);
    }
    for (const entry of [...d.selectedTools]) {
      if (!known.has(entry)) d.selectedTools.delete(entry);
    }
    d.pickersLoaded = true;
  }

  body.innerHTML = `
    <div class="ag-tools-summary" data-tools-summary></div>
    <div class="ag-tools-layout">
      <div class="ag-tools-main">
        <tf-searchbox data-tools-search placeholder="${escapeAttr(t('tools_search_placeholder'))}" debounce="150"></tf-searchbox>
        <div data-addon-groups></div>
        <div class="ag-core-subhead">${escapeHtml(t('tools_catalog_head'))}</div>
        <div data-package-groups></div>
        <div class="ag-core-subhead">${escapeHtml(t('tools_core_head'))}</div>
        <div data-core-groups></div>
      </div>
      <aside class="ag-tools-side">
        <section class="card agents-section">
          <h3 class="agents-section-title">${sprite('shield')} ${escapeHtml(t('tools_how_works'))}</h3>
          <p class="agents-field-hint">${escapeHtml(t('tools_how_works_body'))}</p>
        </section>
        <section class="card agents-section" data-skills-panel></section>
      </aside>
    </div>
  `;

  renderAddonGroups();
  renderPackageGroups();
  renderCoreGroups();
  renderSkillsPanel();
  updateToolsSummary();

  body.querySelector('[data-tools-search]')?.addEventListener('search', (e) => {
    d.toolsSearch = String(e.detail?.value ?? '').toLowerCase();
    renderAddonGroups();
    renderPackageGroups();
    renderCoreGroups();
  });

  renderSaveBar(body);
  updateSaveBar();
}

function toolRowHtml(name, description, checked, disabled, notInstalled) {
  return `
    <div class="agents-tool-item ${notInstalled ? 'is-not-installed' : ''}">
      <span class="agents-tool-name mono">${escapeHtml(name)}</span>
      <span class="agents-tool-desc" title="${escapeAttr(description || '')}">${escapeHtml(description || '')}</span>
      <tf-toggle data-tool="${escapeAttr(name)}" ${checked ? 'checked' : ''} ${disabled ? 'disabled' : ''}></tf-toggle>
    </div>
  `;
}

function groupMatchesSearch(group, search) {
  if (!search) return true;
  const hay = `${group.display_name || ''} ${group.addon_id} ${group.description || ''}`.toLowerCase();
  if (hay.includes(search)) return true;
  return group.tools.some((tool) => `${tool.name} ${tool.description || ''}`.toLowerCase().includes(search));
}

function addonGroupHtml(group, opts = {}) {
  const d = state.detail;
  const search = d.toolsSearch;
  if (!groupMatchesSearch(group, search)) return '';
  const wildcard = `${group.addon_id}.*`;
  const wildcardOn = d.selectedTools.has(wildcard);
  const { title, subtitle } = addonGroupLabel(group);
  const visibleTools = search
    ? group.tools.filter((tool) => `${tool.name} ${tool.description || ''}`.toLowerCase().includes(search))
    : group.tools;
  const headBadges = opts.notInstalled
    ? `<span class="ag-install-hint">${sprite('alert')} ${escapeHtml(t('tools_requires_install'))}</span>`
    : `<tf-chip status="accent">${escapeHtml(t('tools_group_instance'))}</tf-chip>`;
  const anySelected = wildcardOn || group.tools.some((tool) => d.selectedTools.has(tool.name));
  return `
    <div class="agents-tool-group ${opts.notInstalled ? 'is-not-installed' : ''} ${anySelected ? 'is-selected' : ''}" data-group="${escapeAttr(group.addon_id)}">
      <div class="agents-tool-group-head" data-group-head role="button" tabindex="0">
        <div class="agents-tool-group-meta">
          <div class="agents-tool-group-title">
            <span class="agents-tool-group-ico">${sprite(opts.icon || 'puzzle')}</span>
            ${escapeHtml(title)}
            ${headBadges}
          </div>
          <span class="agents-tool-group-id mono">${escapeHtml(opts.subline || group.addon_id)}</span>
          <span class="agents-tool-group-sub" title="${escapeAttr(subtitle)}">${escapeHtml(subtitle)}</span>
        </div>
        ${opts.notInstalled
          ? `<tf-button variant="secondary" size="sm" icon="download" data-install-package="${escapeAttr(group.addon_id)}">${escapeHtml(t('tools_install_cta'))}</tf-button>`
          : `<tf-toggle data-group-toggle="${escapeAttr(wildcard)}" title="${escapeAttr(t('tool_wildcard_hint'))}" ${wildcardOn ? 'checked' : ''}></tf-toggle>`}
        <span class="agents-tool-chev">${sprite('chevron-down')}</span>
      </div>
      <div class="agents-tool-group-body" hidden>
        ${visibleTools.map((tool) => toolRowHtml(tool.name, tool.description, d.selectedTools.has(tool.name), wildcardOn, !!opts.notInstalled)).join('')}
      </div>
    </div>
  `;
}

function renderAddonGroups() {
  const host = document.querySelector('#ag-detail-body [data-addon-groups]');
  const d = state.detail;
  if (!host || !d) return;
  const installed = d.catalog.addons.filter((g) => g.installed !== false);
  host.innerHTML = installed.map((group) => addonGroupHtml(group, {
    icon: 'puzzle',
    subline: group.addon_id,
  })).join('') || `<div class="agents-tree-empty">${escapeHtml(t('tools_addons_none_installed'))}</div>`;
  wireGroupToggles(host);
  wireInstallButtons(host);
}

function renderPackageGroups() {
  const host = document.querySelector('#ag-detail-body [data-package-groups]');
  const d = state.detail;
  if (!host || !d) return;
  const packages = d.catalog.addons.filter((g) => g.installed === false);
  host.innerHTML = packages.map((group) => addonGroupHtml(group, {
    icon: 'layers',
    notInstalled: true,
    subline: t('tools_package_line', { id: group.addon_id }),
  })).join('') || `<div class="agents-tree-empty">${escapeHtml(t('tools_packages_all_installed'))}</div>`;
  wireGroupToggles(host, { readOnly: true });
  wireInstallButtons(host);
}

function renderCoreGroups() {
  const host = document.querySelector('#ag-detail-body [data-core-groups]');
  const d = state.detail;
  if (!host || !d) return;
  const byName = new Map(d.catalog.core.map((tool) => [tool.name, tool]));
  const groupedIds = new Set();
  let html = CORE_TOOL_GROUPS.map((g) => {
    const tools = [];
    for (const name of g.names || []) {
      const tool = byName.get(name);
      if (tool) { tools.push(tool); groupedIds.add(name); }
    }
    for (const [name, tool] of byName) {
      if (!groupedIds.has(name) && (g.prefixes || []).some((p) => name.startsWith(p))) {
        tools.push(tool);
        groupedIds.add(name);
      }
    }
    if (!tools.length) return '';
    const anySelected = tools.some((tool) => d.selectedTools.has(tool.name));
    const visibleTools = d.toolsSearch
      ? tools.filter((tool) => `${tool.name} ${tool.description || ''}`.toLowerCase().includes(d.toolsSearch))
      : tools;
    if (!visibleTools.length) return '';
    return `
      <div class="agents-tool-group ${anySelected ? 'is-selected' : ''}" data-core-group="${escapeAttr(g.id)}">
        <div class="agents-tool-group-head" data-group-head role="button" tabindex="0">
          <div class="agents-tool-group-meta">
            <div class="agents-tool-group-title">
              <span class="agents-tool-group-ico">${sprite(g.icon)}</span>
              ${escapeHtml(t(g.titleKey))}
            </div>
            <span class="agents-tool-group-sub">${escapeHtml(t(g.subKey))}</span>
          </div>
          <span class="agents-tool-chev">${sprite('chevron-down')}</span>
        </div>
        <div class="agents-tool-group-body">
          ${visibleTools.map((tool) => toolRowHtml(tool.name, tool.description, d.selectedTools.has(tool.name), false, false)).join('')}
        </div>
      </div>
    `;
  }).join('');
  const otherTools = d.catalog.core.filter((tool) => !groupedIds.has(tool.name));
  if (otherTools.length && (!d.toolsSearch || otherTools.some((tool) => `${tool.name} ${tool.description || ''}`.toLowerCase().includes(d.toolsSearch)))) {
    html += `
      <div class="agents-tool-group" data-core-group="other">
        <div class="agents-tool-group-head" data-group-head role="button" tabindex="0">
          <div class="agents-tool-group-meta">
            <div class="agents-tool-group-title">
              <span class="agents-tool-group-ico">${sprite('settings')}</span>
              ${escapeHtml(t('tools_core_other'))}
            </div>
          </div>
          <span class="agents-tool-chev">${sprite('chevron-down')}</span>
        </div>
        <div class="agents-tool-group-body">
          ${otherTools.filter((tool) => !d.toolsSearch || `${tool.name} ${tool.description || ''}`.toLowerCase().includes(d.toolsSearch)).map((tool) => toolRowHtml(tool.name, tool.description, d.selectedTools.has(tool.name), false, false)).join('')}
        </div>
      </div>
    `;
  }
  host.innerHTML = html;
  wireGroupToggles(host);
}

// Delegated handlers for collapsible heads + toggles. Re-attached per render —
// the host element itself is replaced, so no double-binding can occur.
function wireGroupToggles(host, { readOnly = false } = {}) {
  if (!host) return;
  // The containers persist across search re-renders; guard so re-render does
  // not stack a second delegated handler (a toggle would then fire twice).
  const guard = readOnly ? 'ro' : 'rw';
  if (host.dataset.wired === guard) return;
  host.dataset.wired = guard;
  host.addEventListener('click', (e) => {
    if (e.target.closest('tf-toggle') || e.target.closest('tf-button')) return;
    const head = e.target.closest('[data-group-head]');
    if (!head) return;
    const group = head.closest('.agents-tool-group');
    const bodyEl = group?.querySelector('.agents-tool-group-body');
    if (bodyEl) {
      bodyEl.hidden = !bodyEl.hidden;
      group.classList.toggle('is-open', !bodyEl.hidden);
    }
  });
  if (readOnly) return;
  host.addEventListener('change', (e) => {
    const d = state.detail;
    if (!d) return;
    const groupToggle = e.target.closest('tf-toggle[data-group-toggle]');
    if (groupToggle) {
      const wildcard = groupToggle.dataset.groupToggle;
      const checked = e.detail?.checked ?? groupToggle.hasAttribute('checked');
      if (checked) d.selectedTools.add(wildcard);
      else d.selectedTools.delete(wildcard);
      // A wildcard covers the whole addon — individual rows lock while it is on.
      const group = groupToggle.closest('.agents-tool-group');
      group?.querySelectorAll('tf-toggle[data-tool]').forEach((tg) => {
        if (checked) tg.setAttribute('disabled', '');
        else tg.removeAttribute('disabled');
      });
      markDirty();
      updateToolsSummary();
      updateDetailTabCounts();
      return;
    }
    const toolToggle = e.target.closest('tf-toggle[data-tool]');
    if (!toolToggle) return;
    if (toolToggle.closest('.is-not-installed')) return;
    const name = toolToggle.dataset.tool;
    const checked = e.detail?.checked ?? toolToggle.hasAttribute('checked');
    if (checked) d.selectedTools.add(name);
    else d.selectedTools.delete(name);
    markDirty();
    updateToolsSummary();
    updateDetailTabCounts();
  });
}

function wireInstallButtons(host) {
  host?.querySelectorAll('[data-install-package]').forEach((btn) => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      window.Router?.navigate('addons', { installPackage: btn.dataset.installPackage });
    });
  });
}

function updateToolsSummary() {
  const host = document.querySelector('#ag-detail-body [data-tools-summary]');
  const d = state.detail;
  if (!host || !d) return;
  const wildcards = [...d.selectedTools].filter((entry) => entry.endsWith('.*'));
  const exactAddon = [...d.selectedTools].filter((entry) => !entry.startsWith('core.') && !entry.endsWith('.*'));
  const coreCount = [...d.selectedTools].filter((entry) => entry.startsWith('core.')).length;
  host.innerHTML = `
    ${sprite('bolt')}
    ${escapeHtml(t('tools_summary_selected', {
      selected: exactAddon.length + coreCount,
      addon: exactAddon.length,
      core: coreCount,
    }))}
    ${wildcards.map((w) => `<tf-chip status="accent" class="mono">${escapeHtml(w)}</tf-chip>`).join('')}
  `;
}

// --- Skills panel ------------------------------------------------------------

function allSkillTags() {
  const set = new Set();
  for (const skill of state.detail?.skills ?? []) {
    for (const tag of parseStringArray(skill.tags_json)) set.add(tag);
  }
  return [...set].sort();
}

function renderSkillsPanel() {
  const d = state.detail;
  const host = document.querySelector('#ag-detail-body [data-skills-panel]');
  if (!host || !d) return;
  const tags = allSkillTags();
  const skills = d.skills;

  if (!skills.length && !tags.length) {
    host.innerHTML = `<h3 class="agents-section-title">${sprite('sparkle')} ${escapeHtml(t('skills_section'))}</h3><div class="agents-tree-empty">${escapeHtml(t('skills_empty'))}</div>`;
    return;
  }

  host.innerHTML = `
    <h3 class="agents-section-title">${sprite('sparkle')} ${escapeHtml(t('skills_section'))}</h3>
      ${tags.length ? `
        <div class="agents-field-hint">${escapeHtml(t('skills_by_tag'))}</div>
        ${tags.map((tag) => `
          <label class="agents-skill-row">
            <tf-checkbox data-skill-tag="${escapeAttr(tag)}" ${d.skillTags.has(tag) ? 'checked' : ''}></tf-checkbox>
            <tf-chip status="info">#${escapeHtml(tag)}</tf-chip>
          </label>
        `).join('')}
      ` : ''}
      ${skills.length ? `
        <div class="agents-field-hint">${escapeHtml(t('skills_by_name'))}</div>
        ${skills.map((s) => `
          <label class="agents-skill-row">
            <tf-checkbox data-skill-name="${escapeAttr(s.name)}" ${d.skillNames.has(s.name) ? 'checked' : ''}></tf-checkbox>
            <span class="agents-skill-name">${escapeHtml(s.name)}</span>
            <span class="agents-skill-desc" title="${escapeAttr(s.description || '')}">${escapeHtml(s.description || '')}</span>
          </label>
        `).join('')}
      ` : ''}
    </section>
  `;

  host.addEventListener('change', (e) => {
    const cb = e.target.closest('tf-checkbox[data-skill-name], tf-checkbox[data-skill-tag]');
    if (!cb) return;
    const checked = e.detail?.checked ?? cb.hasAttribute('checked');
    if (cb.dataset.skillName !== undefined) {
      if (checked) d.skillNames.add(cb.dataset.skillName);
      else d.skillNames.delete(cb.dataset.skillName);
    } else if (cb.dataset.skillTag !== undefined) {
      if (checked) d.skillTags.add(cb.dataset.skillTag);
      else d.skillTags.delete(cb.dataset.skillTag);
    }
    markDirty();
  });
}

function addonGroupLabel(group) {
  const title = String(group.display_name ?? '').trim() || group.addon_id;
  const subtitle = String(group.description ?? '').trim() || t('tools_group_no_description');
  return { title, subtitle };
}

// =============================================================================
// Tab: Testowanie (A04) — sandbox chat + live run timeline
// =============================================================================

function renderTestTab(body) {
  const d = state.detail;
  if (!d || !body) return;

  body.innerHTML = `
    <div class="agents-pg-banner ag-test-banner">
      ${sprite('info')} <span>${escapeHtml(t('pg_banner'))}</span>
    </div>
    <div class="ag-test-cols">
      <section class="card agents-pg-chat">
        <div class="agents-pg-panel-head">
          <span class="agents-pg-panel-title">${sprite('message')} ${escapeHtml(t('pg_chat_title'))}</span>
          <tf-chip status="accent" dot>${escapeHtml(t('pg_session_chip'))}</tf-chip>
          <tf-button variant="ghost" size="sm" icon="refresh" data-pg-new-session>${escapeHtml(t('pg_new_session'))}</tf-button>
        </div>
        <div class="agents-pg-msgs" data-pg-msgs></div>
        <tf-chat-composer placeholder="${escapeAttr(t('pg_placeholder'))}"></tf-chat-composer>
      </section>
      <div class="ag-test-side">
        <div data-pg-widget></div>
        <section class="card agents-pg-run">
          <div class="agents-pg-panel-head">
            <span class="agents-pg-panel-title">${sprite('clock')} ${escapeHtml(t('pg_run_title'))}</span>
          </div>
          <div class="agents-pg-timeline" data-pg-timeline></div>
        </section>
      </div>
    </div>
  `;

  if (!state.pg || state.pg.agentId !== d.agentId) {
    state.pg = {
      agentId: d.agentId,
      messages: [{ role: 'assistant', content: t('pg_greeting'), time: nowTime() }],
      steps: [],
      widget: null,
      unsub: null,
      runId: null,
      busy: false,
    };
  }

  // The interactive widget owns question/permission cards and per-run cancel;
  // its CustomEvents are forwarded to the binary protocol exactly like the
  // chat bridge does (agent-activity-bridge contract).
  const widget = document.createElement('tf-agent-activity');
  widget.labels = activityLabels();
  widget.addEventListener('agent-reply', async (e) => {
    const { runId, interactionId, answer } = e.detail || {};
    try {
      await ApiBinary.action('agentRunReplyRequest', { runId, questionId: interactionId, answer });
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message ?? err}`, 'error');
    }
  });
  widget.addEventListener('agent-permission', async (e) => {
    const { runId, interactionId, decision } = e.detail || {};
    try {
      await ApiBinary.action('agentPermissionReplyRequest', { runId, requestId: interactionId, decision });
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message ?? err}`, 'error');
    }
  });
  widget.addEventListener('agent-cancel', async (e) => {
    const { runId } = e.detail || {};
    try {
      const resp = await ApiBinary.action('agentRunCancelRequest', { runId });
      if (resp && (resp.cancelled === true || resp.cancelled === 'true')) {
        widget.setRunStatus(runId, 'cancelled');
      }
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message ?? err}`, 'error');
    }
  });
  body.querySelector('[data-pg-widget]')?.appendChild(widget);
  state.pg.widget = widget;

  body.querySelector('[data-pg-new-session]')?.addEventListener('click', () => resetPlaygroundSession());
  body.querySelector('tf-chat-composer')?.addEventListener('send', (e) => {
    const text = String(e.detail?.text ?? '').trim();
    if (text) startPlaygroundRun(text);
  });

  renderPlaygroundMessages();
  renderPlaygroundTimeline();
}

function nowTime() {
  return new Date().toLocaleTimeString(I18n.getLanguage(), { hour: '2-digit', minute: '2-digit' });
}

function renderPlaygroundMessages() {
  const pg = state.pg;
  const host = document.querySelector('#ag-detail-body [data-pg-msgs]');
  if (!pg || !host) return;
  const agent = state.detail?.agent;
  const bubbles = pg.messages.map((m) => `
    <tf-chat-bubble role="${m.role === 'user' ? 'user' : 'assistant'}"
      sender="${escapeAttr(m.role === 'user' ? '' : (agent?.display_name || agent?.name || ''))}"
      time="${escapeAttr(m.time || '')}">${escapeHtml(m.content)}</tf-chat-bubble>
  `).join('');
  const typing = pg.busy
    ? `<tf-chat-bubble role="assistant" streaming sender="${escapeAttr(agent?.display_name || agent?.name || '')}">${escapeHtml(t('pg_running'))}</tf-chat-bubble>`
    : '';
  host.innerHTML = bubbles + typing;
  host.scrollTop = host.scrollHeight;
  const composer = document.querySelector('#ag-detail-body tf-chat-composer');
  if (composer) {
    if (pg.busy) composer.setAttribute('disabled', '');
    else composer.removeAttribute('disabled');
  }
}

function renderPlaygroundTimeline() {
  const pg = state.pg;
  const host = document.querySelector('#ag-detail-body [data-pg-timeline]');
  if (!pg || !host) return;
  host.innerHTML = TfAgentActivity.renderTimeline(pg.steps, activityLabels());
  host.scrollTop = host.scrollHeight;
}

async function startPlaygroundRun(prompt) {
  const pg = state.pg;
  if (!pg || pg.busy) return;
  pg.messages.push({ role: 'user', content: prompt, time: nowTime() });
  pg.busy = true;
  pg.steps = [];
  renderPlaygroundMessages();
  renderPlaygroundTimeline();

  let runId = null;
  try {
    const resp = await ApiBinary.one('agentRunStartRequest', { agentId: pg.agentId, prompt });
    runId = resp.runId ?? resp.run_id ?? null;
  } catch (err) {
    pg.busy = false;
    pg.messages.push({ role: 'assistant', content: `${t('pg_start_failed')}: ${err.message}`, time: nowTime() });
    renderPlaygroundMessages();
    return;
  }
  if (!runId) {
    pg.busy = false;
    pg.messages.push({ role: 'assistant', content: t('pg_start_failed'), time: nowTime() });
    renderPlaygroundMessages();
    return;
  }
  pg.runId = runId;

  // A slow safety-net poll runs regardless of stream state: broadcasts have no
  // replay, so a run that finishes before the subscription reaches the server
  // never delivers `child_finished` and the stream is never `push_end`-ed —
  // without this the playground would hang on "running…" forever.
  pollPlaygroundRun(pg, runId);

  // The run publishes ChildFinished to its OWN scope on completion (run_manager
  // publish_child_finished), so the root run's terminal event arrives here too.
  try {
    pg.unsub = await ApiBinary.subscribe(
      'agentRunEventsSubscribeRequest',
      { scopeKind: 'run', scopeId: runId },
      {
        onChunk: (body) => {
          if (!body || body.variant !== 'AgentRunEvent') return;
          if (state.pg !== pg || pg.runId !== runId) return;
          pg.widget?.applyEvent(body);
          const step = TfAgentActivity.stepsFromEvents([body], activityLabels())[0];
          step.ts = new Date().toLocaleTimeString();
          pg.steps.push(step);
          renderPlaygroundTimeline();
          const eventRunId = body.run_id || body.runId;
          if (body.kind === 'child_finished' && eventRunId === runId) {
            finishPlaygroundRun(pg, runId);
          }
        },
        onError: () => {},
        onEnd: () => {
          if (state.pg === pg && pg.runId === runId && pg.busy) {
            finishPlaygroundRun(pg, runId);
          }
        },
      },
    );
    // Fast-run race: the run may already be terminal by the time the subscribe
    // handle resolves, and its `child_finished` broadcast was missed. Reconcile
    // once from the durable row and close the now-useless stream.
    if (state.pg !== pg || pg.runId !== runId || !pg.busy) {
      if (pg.unsub) { pg.unsub(); pg.unsub = null; }
      return;
    }
    try {
      const resp = await ApiBinary.one('agentRunDetailRequest', { runId });
      const run = JSON.parse(resp.runJson ?? resp.run_json ?? 'null');
      if (run && TERMINAL_RUN_STATUSES.includes(run.status)) {
        finishPlaygroundRun(pg, runId);
      }
    } catch {
      // The safety-net poll will retry; a single failed reconcile is harmless.
    }
  } catch (err) {
    // Without a live stream the safety-net poll is the only terminal signal.
    console.warn('agents: run-events subscribe failed', err);
  }
}

// Polls the durable run row until terminal. Runs as a safety net beside the
// event stream; self-terminates when the run finishes, the session resets, or
// the test tab tears down (all three flip one of the guarded conditions).
async function pollPlaygroundRun(pg, runId) {
  while (state.pg === pg && pg.runId === runId && pg.busy) {
    await new Promise((resolve) => setTimeout(resolve, 4000));
    if (state.pg !== pg || pg.runId !== runId || !pg.busy) return;
    let run = null;
    try {
      const resp = await ApiBinary.one('agentRunDetailRequest', { runId });
      run = JSON.parse(resp.runJson ?? resp.run_json ?? 'null');
    } catch {
      continue;
    }
    if (run && TERMINAL_RUN_STATUSES.includes(run.status)) {
      finishPlaygroundRun(pg, runId);
      return;
    }
  }
}

async function finishPlaygroundRun(pg, runId) {
  if (!pg.busy) return;
  pg.busy = false;
  if (pg.unsub) { pg.unsub(); pg.unsub = null; }
  let run = null;
  try {
    const resp = await ApiBinary.one('agentRunDetailRequest', { runId });
    run = JSON.parse(resp.runJson ?? resp.run_json ?? 'null');
  } catch (err) {
    pg.messages.push({ role: 'assistant', content: `${t('run_detail_failed')}: ${err.message}`, time: nowTime() });
    renderPlaygroundMessages();
    return;
  }
  const content = run?.result
    || (run?.exit_reason ? `${runStatusLabel(run.status)} · ${run.exit_reason}` : t('pg_no_result'));
  pg.messages.push({ role: 'assistant', content, time: nowTime() });
  renderPlaygroundMessages();
}

async function resetPlaygroundSession() {
  const pg = state.pg;
  if (!pg) return;
  if (pg.busy && pg.runId) {
    try {
      await ApiBinary.action('agentRunCancelRequest', { runId: pg.runId });
    } catch {
      // A cancel race with a finishing run is fine — the session resets anyway.
    }
  }
  if (pg.unsub) { pg.unsub(); pg.unsub = null; }
  pg.busy = false;
  pg.runId = null;
  pg.steps = [];
  pg.messages = [{ role: 'assistant', content: t('pg_greeting'), time: nowTime() }];
  renderPlaygroundMessages();
  renderPlaygroundTimeline();
}

function teardownPg() {
  const pg = state.pg;
  if (!pg) return;
  if (pg.unsub) { pg.unsub(); pg.unsub = null; }
  state.pg = null;
}

// =============================================================================
// Tab: Przebiegi (A05) — per-agent run history + drill-in
// =============================================================================

async function renderAgentRunsTab(body) {
  const d = state.detail;
  if (!d || !body) return;
  body.innerHTML = `
    <section class="card agents-card">
      <div class="agents-toolbar">
        <tf-select id="agent-runs-filter-status" class="agents-filter" value="${escapeAttr(d.agentRunsFilter)}">
          <option value="all">${escapeHtml(t('runs_filter_status_all'))}</option>
          <option value="active">${escapeHtml(t('runs_filter_status_active'))}</option>
          <option value="completed">${escapeHtml(t('run_status_completed'))}</option>
          <option value="failed">${escapeHtml(t('run_status_failed'))}</option>
        </tf-select>
        <tf-button variant="ghost" icon="refresh" id="agent-runs-refresh">${escapeHtml(t('refresh'))}</tf-button>
      </div>
      <div id="agent-runs-table-host" class="agents-runs-host"></div>
      <div id="agent-runs-detail-host" class="agents-run-detail"></div>
    </section>
  `;
  body.querySelector('#agent-runs-refresh')?.addEventListener('click', () => loadAgentRuns());
  body.querySelector('#agent-runs-filter-status')?.addEventListener('change', (e) => {
    const dd = state.detail;
    if (!dd) return;
    dd.agentRunsFilter = e.detail?.value ?? e.target.value ?? 'all';
    renderAgentRunsTable();
  });
  await loadAgentRuns();
}

async function loadAgentRuns() {
  const d = state.detail;
  const host = byId('agent-runs-table-host');
  if (!d || !host) return;
  host.innerHTML = `<div class="agents-tree-empty">${escapeHtml(t('runs_loading'))}</div>`;
  try {
    const resp = await ApiBinary.one('agentRunsListRequest', { agentId: d.agentId });
    const rows = JSON.parse(resp.runsJson ?? resp.runs_json ?? '[]');
    d.agentRuns = Array.isArray(rows) ? rows : [];
  } catch (err) {
    if (state.detail === d && byId('agent-runs-table-host')) {
      host.innerHTML = `<div class="agents-form-error">${escapeHtml(`${t('runs_failed')}: ${err.message}`)}</div>`;
    }
    return;
  }
  d.runsLoaded = true;
  renderAgentRunsTable();
}

function filteredAgentRuns() {
  const d = state.detail;
  if (!d) return [];
  const f = d.agentRunsFilter;
  return d.agentRuns.filter((r) => {
    if (f === 'all') return true;
    if (f === 'active') return !TERMINAL_RUN_STATUSES.includes(r.status);
    return r.status === f;
  });
}

function renderAgentRunsTable() {
  const d = state.detail;
  const host = byId('agent-runs-table-host');
  if (!d || !host) return;
  const runs = filteredAgentRuns();
  if (!runs.length) {
    host.innerHTML = `<tf-empty-state icon="clock" title="${escapeAttr(t('runs_empty'))}"></tf-empty-state>`;
    return;
  }
  host.innerHTML = `
    <tf-table id="agent-runs-table" sortable>
      <tf-column key="status" label="${escapeAttr(t('runs_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="iterations" label="${escapeAttr(t('runs_col_iterations'))}" renderer="num" sortable></tf-column>
      <tf-column key="tokens" label="${escapeAttr(t('runs_col_tokens'))}" renderer="num" sortable></tf-column>
      <tf-column key="exit_reason" label="${escapeAttr(t('runs_col_exit'))}"></tf-column>
      <tf-column key="created" label="${escapeAttr(t('runs_col_created'))}" sortable></tf-column>
    </tf-table>
  `;
  const table = host.querySelector('#agent-runs-table');
  table.rows = runs.map((r) => ({
    _id: r.id,
    _status: r.status,
    status: { status: RUN_STATUS_CHIP[r.status] || 'info', label: runStatusLabel(r.status) },
    iterations: r.iterations ?? 0,
    tokens: r.total_tokens ?? 0,
    exit_reason: r.exit_reason || '—',
    created: formatTimestamp(r.created_at),
  }));
  table.rowActions = (row) => buildRunRowActions(row, 'agent');
  table.addEventListener('row-click', (e) => {
    const id = e.detail?.row?._id;
    if (id) openRunDetail(id, 'agent-runs-detail-host', 'agent');
  });
}

async function cancelRun(runId, reload) {
  if (!runId) return;
  try {
    const resp = await ApiBinary.action('agentRunCancelRequest', { runId });
    if (resp && (resp.cancelled === true || resp.cancelled === 'true')) {
      toast(t('run_cancel_ok'), 'success');
    }
    if (reload === 'global') await loadRunsTab();
    else if (reload === 'agent') await loadAgentRuns();
  } catch (err) {
    toast(`${t('run_cancel_failed')}: ${err.message}`, 'error');
  }
}

// =============================================================================
// Run detail (shared by the global Runs tab and the per-agent Przebiegi tab).
// `scope` picks which hosts/state the drill-in writes to so two details can
// never fight over one DOM node or one subscription handle.
// =============================================================================

async function openRunDetail(runId, hostId = 'runs-detail-host', scope = 'global') {
  const d = scope === 'agent' ? state.detail : null;
  const token = scope === 'agent' ? ++d.agentRunsToken : ++state.runsSubToken;
  const stepsMap = scope === 'agent' ? d.agentRunSteps : state.runSteps;
  if (scope === 'global') {
    if (state.runsUnsub) { state.runsUnsub(); state.runsUnsub = null; }
    state.selectedRunId = runId;
  } else {
    if (d.agentRunsUnsub) { d.agentRunsUnsub(); d.agentRunsUnsub = null; }
    d.selectedAgentRunId = runId;
  }

  const host = byId(hostId);
  if (!host) return;
  host.innerHTML = `<div class="agents-tree-empty">${escapeHtml(t('runs_loading'))}</div>`;
  let run = null;
  try {
    const resp = await ApiBinary.one('agentRunDetailRequest', { runId });
    run = JSON.parse(resp.runJson ?? resp.run_json ?? 'null');
  } catch (err) {
    const stale = scope === 'agent'
      ? (state.detail !== d || d.selectedAgentRunId !== runId || d.agentRunsToken !== token)
      : (state.selectedRunId !== runId || state.runsSubToken !== token);
    if (!stale) {
      host.innerHTML = `<div class="agents-form-error">${escapeHtml(`${t('run_detail_failed')}: ${err.message}`)}</div>`;
    }
    return;
  }
  const staleAfterFetch = scope === 'agent'
    ? (state.detail !== d || d.selectedAgentRunId !== runId || d.agentRunsToken !== token)
    : (state.selectedRunId !== runId || state.runsSubToken !== token);
  if (!run || staleAfterFetch) return;
  // Seed the timeline from run_log (durable record), then append live events.
  stepsMap.set(runId, runLogToSteps(run));
  host.innerHTML = renderRunDetail(run);
  // Wire the detail actions imperatively — innerHTML-injected scripts never run.
  host.querySelector('[data-run-copy-id]')?.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(String(run.id ?? ''));
      toast(t('run_copy_ok'), 'success');
    } catch { /* clipboard denied — nothing to recover */ }
  });
  host.querySelector('[data-run-rerun]')?.addEventListener('click', async () => {
    if (!run.prompt) return;
    if (scope === 'agent' && d) {
      await switchDetailTab('test');
    } else {
      switchTopTab('agents');
    }
    // Give the test tab one tick to build its composer before starting.
    setTimeout(() => startPlaygroundRun(String(run.prompt)), 50);
  });

  // Live updates for this run scope: append each AgentRunEvent as a step.
  if (!TERMINAL_RUN_STATUSES.includes(run.status)) {
    ApiBinary.subscribe(
      'agentRunEventsSubscribeRequest',
      { scopeKind: 'run', scopeId: runId },
      {
        onChunk: (body) => {
          if (!body || body.variant !== 'AgentRunEvent') return;
          const stillCurrent = scope === 'agent'
            ? (state.detail === d && d.selectedAgentRunId === runId && d.agentRunsToken === token)
            : (state.selectedRunId === runId && state.runsSubToken === token);
          if (!stillCurrent) return;
          const steps = stepsMap.get(runId) || [];
          const step = TfAgentActivity.stepsFromEvents([body], activityLabels())[0];
          step.ts = new Date().toLocaleTimeString();
          steps.push(step);
          stepsMap.set(runId, steps);
          const tl = byId(hostId)?.querySelector('[data-run-timeline]');
          if (tl) tl.innerHTML = TfAgentActivity.renderTimeline(steps, activityLabels());
        },
        onError: () => {},
        onEnd: () => {
          const stillCurrent = scope === 'agent'
            ? (state.detail === d && d.agentRunsToken === token)
            : (state.runsSubToken === token);
          if (stillCurrent) {
            if (scope === 'agent') d.agentRunsUnsub = null;
            else state.runsUnsub = null;
          }
        },
      },
    ).then((unsub) => {
      const superseded = scope === 'agent'
        ? (state.detail !== d || d.agentRunsToken !== token)
        : (state.runsSubToken !== token);
      // A newer request (or unmount) already superseded this one — do not
      // publish the handle; close the stream immediately.
      if (superseded) { unsub(); return; }
      if (scope === 'agent') d.agentRunsUnsub = unsub;
      else state.runsUnsub = unsub;
    }).catch(() => {});
  }
}

// run_log is a JSON array of harness step objects. Map them onto the shared
// timeline's step shape so live events and durable history render identically.
function runLogToSteps(run) {
  let raw = [];
  try {
    const parsed = JSON.parse(run.run_log || '[]');
    if (Array.isArray(parsed)) raw = parsed;
  } catch {
    raw = [];
  }
  return raw.map((s) => ({
    tone: s.status === 'error' || s.status === 'failed' ? 'err' : s.status === 'ok' ? 'ok' : 'info',
    kind: String(s.kind || s.type || t('run_step')),
    detail: String(s.detail || s.message || s.tool || ''),
    ts: s.at || s.timestamp ? formatTimestamp(String(s.at || s.timestamp)) : '',
  }));
}

function renderRunDetail(run) {
  const d = state.detail;
  const inAgentTab = !!(d && byId('agent-runs-detail-host'));
  const stepsMap = inAgentTab ? d.agentRunSteps : state.runSteps;
  const steps = stepsMap.get(run.id) || [];
  const actions = `
    <div class="agents-run-meta">
      <tf-button variant="ghost" size="sm" icon="copy" data-run-copy-id>${escapeHtml(t('run_copy_id'))}</tf-button>
      ${run.prompt ? `<tf-button variant="ghost" size="sm" icon="play" data-run-rerun>${escapeHtml(t('run_rerun'))}</tf-button>` : ''}
    </div>`;
  const meta = `
    <div class="agents-run-meta">
      <tf-chip status="${RUN_STATUS_CHIP[run.status] || 'info'}">${escapeHtml(runStatusLabel(run.status))}</tf-chip>
      <span>${escapeHtml(t('runs_col_iterations'))}: ${escapeHtml(String(run.iterations ?? 0))}</span>
      <span>${escapeHtml(t('runs_col_tokens'))}: ${escapeHtml(String(run.total_tokens ?? 0))}</span>
      ${run.exit_reason ? `<span>${escapeHtml(t('runs_col_exit'))}: ${escapeHtml(run.exit_reason)}</span>` : ''}
    </div>`;
  const prompt = run.prompt
    ? `<div class="agents-run-block"><span class="tf-label">${escapeHtml(t('run_prompt'))}</span><pre>${escapeHtml(run.prompt)}</pre></div>`
    : '';
  const result = run.result
    ? `<div class="agents-run-block"><span class="tf-label">${escapeHtml(t('run_result'))}</span><pre>${escapeHtml(run.result)}</pre></div>`
    : '';
  const timeline = `<div data-run-timeline>${TfAgentActivity.renderTimeline(steps, activityLabels())}</div>`;
  // Buttons are wired by openRunDetail right after this markup hits the DOM.
  return `${meta}${actions}${prompt}${result}
    <div class="agents-run-block"><span class="tf-label">${escapeHtml(t('run_timeline'))}</span>${timeline}</div>`;
}


function runStatusLabel(status) {
  return RUN_STATUS_CHIP[status] ? t(`run_status_${status}`) : status;
}

// =============================================================================
// Top-level Runs tab — live run list + shared timeline + cancel (ACL per
// principal is enforced server-side; the list only returns the caller's runs
// unless they are an admin).
// =============================================================================

function switchTopTab(tabId) {
  state.topTab = tabId === 'runs' ? 'runs' : 'agents';
  document.querySelectorAll('[data-top-panel]').forEach((panel) => {
    panel.hidden = panel.getAttribute('data-top-panel') !== state.topTab;
  });
  if (state.topTab === 'runs') loadRunsTab();
  else {
    // Leaving Runs: cancel the detail stream and invalidate any in-flight
    // subscribe so its resolved handle is discarded rather than published.
    state.runsSubToken += 1;
    if (state.runsUnsub) { state.runsUnsub(); state.runsUnsub = null; }
  }
}

async function loadRunsTab() {
  const host = byId('runs-table-host');
  if (host) host.innerHTML = `<div class="agents-tree-empty">${escapeHtml(t('runs_loading'))}</div>`;
  try {
    const resp = await ApiBinary.one('agentRunsListRequest', {});
    const rows = JSON.parse(resp.runsJson ?? resp.runs_json ?? '[]');
    state.runs = Array.isArray(rows) ? rows : [];
  } catch (err) {
    if (host) host.innerHTML = `<div class="agents-form-error">${escapeHtml(`${t('runs_failed')}: ${err.message}`)}</div>`;
    return;
  }
  renderRunsTable();
}

function filteredRuns() {
  const f = state.runsStatusFilter;
  return state.runs.filter((r) => {
    if (f === 'all') return true;
    if (f === 'active') return !TERMINAL_RUN_STATUSES.includes(r.status);
    return r.status === f;
  });
}

function renderRunsTable() {
  const host = byId('runs-table-host');
  if (!host) return;
  const runs = filteredRuns();
  if (!runs.length) {
    host.innerHTML = `<tf-empty-state icon="clock" title="${escapeAttr(t('runs_empty'))}"></tf-empty-state>`;
    return;
  }
  host.innerHTML = `
    <tf-table id="runs-tab-table" sortable>
      <tf-column key="status" label="${escapeAttr(t('runs_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="agent" label="${escapeAttr(t('runs_col_agent'))}" sortable></tf-column>
      <tf-column key="iterations" label="${escapeAttr(t('runs_col_iterations'))}" renderer="num" sortable></tf-column>
      <tf-column key="tokens" label="${escapeAttr(t('runs_col_tokens'))}" renderer="num" sortable></tf-column>
      <tf-column key="exit_reason" label="${escapeAttr(t('runs_col_exit'))}"></tf-column>
      <tf-column key="created" label="${escapeAttr(t('runs_col_created'))}" sortable></tf-column>
    </tf-table>
  `;
  const table = host.querySelector('#runs-tab-table');
  table.rows = runs.map((r) => ({
    _id: r.id,
    _status: r.status,
    status: { status: RUN_STATUS_CHIP[r.status] || 'info', label: runStatusLabel(r.status) },
    agent: r.agent_id || '—',
    iterations: r.iterations ?? 0,
    tokens: r.total_tokens ?? 0,
    exit_reason: r.exit_reason || '—',
    created: formatTimestamp(r.created_at),
  }));
  table.rowActions = buildRunRowActions;
  table.addEventListener('row-click', (e) => {
    const id = e.detail?.row?._id;
    if (id) openRunDetail(id);
  });
}

function buildRunRowActions(row) {
  const active = !TERMINAL_RUN_STATUSES.includes(row._status);
  if (!active) return null;
  const btn = document.createElement('tf-button');
  btn.setAttribute('variant', 'ghost');
  btn.setAttribute('size', 'sm');
  btn.textContent = t('action_cancel');
  btn.addEventListener('click', () => cancelRun(row._id, 'global'));
  return btn;
}

// =============================================================================
// Wizard (A02 mockup) — CREATE-only 3-step window
// =============================================================================

async function openWizard(agent, { forceCreate = false } = {}) {
  state.wizard?.cleanup();
  state.assist?.cleanup();
  sweepWindows('wizard');
  sweepWindows('assist');
  // A duplicate prefills the create-only wizard from an existing row; there is
  // deliberately no edit mode left — editing happens in the agent detail tabs.
  const mode = 'create';

  const win = document.createElement('tf-window');
  win.setAttribute('title', t(mode === 'create' && !forceCreate ? 'wizard_title_new' : 'wizard_title_duplicate'));
  win.setAttribute('icon', 'bot');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '720');
  win.setAttribute('draggable', '');

  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'agents-wizard';

  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.className = 'agents-editor-footer';
  foot.innerHTML = `
    <div class="agents-footer-left">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
    </div>
    <div class="agents-footer-right">
      <tf-button variant="ghost" data-action="back" style="visibility:hidden;">${escapeHtml(t('wizard_back'))}</tf-button>
      <tf-button variant="primary" icon="chevron-right" data-action="next">${escapeHtml(t('wizard_next'))}</tf-button>
    </div>
  `;
  win.append(body, foot);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  win.setAttribute('data-agents-window', 'wizard');
  backdrop.setAttribute('data-agents-window', 'wizard');
  document.body.append(backdrop, win);

  const cleanup = () => {
    win.remove();
    backdrop.remove();
    if (state.wizard?.win === win) state.wizard = null;
  };
  win.addEventListener('close-request', () => cleanup());
  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'close') cleanup();
  });

  const source = agent && forceCreate ? agent : null;
  const skillsSel = source ? normalizeSkillsSelection(source.skills) : { names: [], tags: [] };
  // Register the wizard BEFORE the data awaits: a double-open must find and
  // clean up this instance instead of stacking a second window behind the
  // async gap.
  const wzPending = {
    win, body, foot, mode, step: 1,
    agent: forceCreate ? agent : null,
    sourceParams: (source?.params && typeof source.params === 'object') ? { ...source.params } : {},
    models: [], catalog: { addons: [], core: [] }, skills: [],
    selectedTools: new Set(source ? (Array.isArray(source.tools) ? source.tools : []) : []),
    selectedSkillNames: new Set(skillsSel.names),
    selectedSkillTags: new Set(skillsSel.tags),
    cleanup,
  };
  state.wizard = wzPending;

  const [models, catalog, skills] = await Promise.all([loadModelOptions(), loadToolsCatalog(), loadSkillList()]);
  if (!win.isConnected || state.wizard !== wzPending) return;
  wzPending.models = models;
  wzPending.catalog = catalog;
  wzPending.skills = skills;

  body.innerHTML = wizardBodyHtml(wzPending.agent, mode, models);
  wireWizard();
  setWizardStep(1);
}

function wizardBodyHtml(agent, mode, models) {
  const modelOptions = [
    `<option value="">${escapeHtml(t('model_inherited'))}</option>`,
    ...models.map((m) => `<option value="${escapeAttr(m.value)}">${escapeHtml(m.label)}</option>`),
  ].join('');

  const personaCards = mode === 'create' && !agent ? `
    <div class="agents-field">
      <span class="tf-label">${escapeHtml(t('persona_label'))}</span>
      <div class="agents-field-hint">${escapeHtml(t('persona_hint'))}</div>
      <div class="agents-persona-grid">
        ${PERSONAS.map((p) => `
          <div class="agents-persona-card" data-persona="${escapeAttr(p.id)}" role="button" tabindex="0">
            <div class="agents-persona-ico">${sprite(p.icon)}</div>
            <div class="agents-persona-name">${escapeHtml(t(`persona_${p.id}_name`))}</div>
            <div class="agents-persona-desc">${escapeHtml(t(`persona_${p.id}_desc`))}</div>
          </div>
        `).join('')}
      </div>
    </div>
  ` : '';

  const iterations = Math.min(Math.max(agent?.max_iterations ?? 25, 1), MAX_ITERATIONS_CAP);

  return `
    <div class="agents-stepper">
      <div class="agents-step" data-step-pill="1"><span class="agents-step-n">1</span>${escapeHtml(t('wizard_step1'))}</div>
      <div class="agents-step-line" data-step-line="1"></div>
      <div class="agents-step" data-step-pill="2"><span class="agents-step-n">2</span>${escapeHtml(t('wizard_step2'))}</div>
      <div class="agents-step-line" data-step-line="2"></div>
      <div class="agents-step" data-step-pill="3"><span class="agents-step-n">3</span>${escapeHtml(t('wizard_step3'))}</div>
    </div>

    <div class="agents-wizard-step" data-step-panel="1">
      <div class="agents-editor-grid">
        <tf-input id="agent-wz-name" label="${escapeAttr(t('label_name'))}"
          value="${escapeAttr(agent?.name || '')}"
          hint="${escapeAttr(t('hint_name'))}"
          maxlength="${NAME_MAX_CHARS}"></tf-input>
        <tf-input id="agent-wz-display-name" label="${escapeAttr(t('label_display_name'))}"
          value="${escapeAttr(agent?.display_name || '')}"></tf-input>
      </div>

      <div class="agents-assist-box">
        <div class="agents-assist-title">${sprite('sparkle')} ${escapeHtml(t('assist_box_title'))}</div>
        <div class="agents-assist-text">${escapeHtml(t('assist_box_text'))}</div>
        <tf-button variant="primary" icon="sparkle" data-open-assist>${escapeHtml(t('assist_box_title'))}</tf-button>
      </div>

      ${personaCards}

      <tf-textarea id="agent-wz-description" label="${escapeAttr(t('label_description'))}"
        rows="3" hint="${escapeAttr(t('hint_description'))}"
        value="${escapeAttr(agent?.description || '')}"></tf-textarea>

      <tf-textarea id="agent-wz-system-prompt" label="${escapeAttr(t('label_system_prompt'))}"
        rows="7" hint="${escapeAttr(t('hint_system_prompt'))}"
        value="${escapeAttr(agent?.system_prompt || '')}"></tf-textarea>
    </div>

    <div class="agents-wizard-step" data-step-panel="2" hidden>
      <div class="agents-field">
        <span class="tf-label">${escapeHtml(t('tools_section'))}</span>
        <div class="agents-field-hint">${escapeHtml(t('tools_section_hint'))}</div>
        <div id="agent-wz-tools" class="agents-tool-groups"></div>
        <div class="agents-field-hint">${escapeHtml(t('tools_footer_hint'))}</div>
      </div>

      <div class="agents-field">
        <span class="tf-label">${escapeHtml(t('skills_section'))}</span>
        <div class="agents-field-hint">${escapeHtml(t('hint_skills'))}</div>
        <div id="agent-wz-skills" class="agents-skill-tree"></div>
      </div>
    </div>

    <div class="agents-wizard-step" data-step-panel="3" hidden>
      <div class="agents-editor-grid">
        <div class="agents-field">
          <tf-select id="agent-wz-model" label="${escapeAttr(t('label_model'))}"
            value="${escapeAttr(agent?.model || '')}">${modelOptions}</tf-select>
          <div class="agents-field-hint">${escapeHtml(t('model_hint'))}</div>
        </div>
        <tf-input id="agent-wz-flow-id" label="${escapeAttr(t('label_flow_id'))}"
          value="${escapeAttr(agent?.flow_id || '')}"
          hint="${escapeAttr(t('hint_flow_id'))}"></tf-input>
      </div>

      <div class="agents-editor-grid">
        <!-- Poziom rozumowania zalezy od MODELU: opcje wypelnia
             refreshReasoningOptions() z katalogu, a gdy model ich nie ma, cale
             pole znika zamiast oferowac ustawienie, ktore backend odrzuci. -->
        <div class="agents-field" id="agent-wz-reasoning-field" hidden>
          <tf-select id="agent-wz-reasoning" label="${escapeAttr(t('label_reasoning_effort'))}"
            value="${escapeAttr(agent?.params?.reasoning_effort || '')}"></tf-select>
          <div class="agents-field-hint">${escapeHtml(t('hint_reasoning_effort'))}</div>
        </div>
        <div class="agents-field">
          <tf-input id="agent-wz-temperature" type="number" min="0" max="2" step="0.1"
            label="${escapeAttr(t('label_temperature'))}"
            value="${escapeAttr(agent?.params?.temperature ?? '')}"></tf-input>
          <div class="agents-field-hint">${escapeHtml(t('hint_temperature'))}</div>
        </div>
      </div>

      <div class="agents-field">
        <span class="tf-label">${escapeHtml(t('behavior_section'))}</span>
        <div class="agents-slider-row">
          <div>
            <div class="agents-slider-name">${escapeHtml(t('label_max_iterations'))}</div>
            <div class="agents-field-hint">${escapeHtml(t('slider_iterations_desc'))}</div>
          </div>
          <tf-slider id="agent-wz-max-iterations" min="1" max="${MAX_ITERATIONS_CAP}" step="1"
            value="${escapeAttr(String(iterations))}"></tf-slider>
          <div class="agents-slider-val" data-iterations-val>${escapeHtml(String(iterations))}</div>
        </div>
      </div>

      <div class="agents-editor-grid agents-limits">
        <tf-input id="agent-wz-timeout-secs" type="number" label="${escapeAttr(t('label_timeout_secs'))}"
          value="${escapeAttr(String(agent?.timeout_secs ?? 600))}" min="1"></tf-input>
        <tf-input id="agent-wz-max-subagents" type="number" label="${escapeAttr(t('label_max_subagents'))}"
          value="${escapeAttr(String(agent?.max_subagents ?? 0))}" min="0"></tf-input>
        <tf-input id="agent-wz-max-spawn-depth" type="number" label="${escapeAttr(t('label_max_spawn_depth'))}"
          value="${escapeAttr(String(agent?.max_spawn_depth ?? 1))}" min="1"></tf-input>
        <tf-select id="agent-wz-on-child-complete" label="${escapeAttr(t('label_on_child_complete'))}"
          value="${escapeAttr(agent?.on_child_complete || 'notify')}">
          <option value="notify">${escapeHtml(t('on_child_complete_notify'))}</option>
          <option value="continue">${escapeHtml(t('on_child_complete_continue'))}</option>
        </tf-select>
      </div>

      <div class="agents-toggle-card">
        <tf-toggle id="agent-wz-routable" ${agent ? (agent.routable ? 'checked' : '') : 'checked'}></tf-toggle>
        <div>
          <div class="agents-toggle-name">${escapeHtml(t('label_routable'))}</div>
          <div class="agents-field-hint">${escapeHtml(t('routable_desc'))}</div>
        </div>
      </div>
      <div class="agents-toggle-card">
        <tf-toggle id="agent-wz-enabled" ${agent ? (agent.is_enabled ? 'checked' : '') : 'checked'}></tf-toggle>
        <div>
          <div class="agents-toggle-name">${escapeHtml(t('label_enabled'))}</div>
          <div class="agents-field-hint">${escapeHtml(t('enabled_desc'))}</div>
        </div>
      </div>
    </div>

    <div class="agents-form-error" data-form-error hidden></div>
  `;
}

function wireWizard() {
  const wz = state.wizard;

  wz.foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') {
      wz.cleanup();
    } else if (btn.dataset.action === 'back') {
      if (wz.step > 1) setWizardStep(wz.step - 1);
    } else if (btn.dataset.action === 'next') {
      const error = validateWizardStep(wz.step);
      if (error) {
        showFormError(error);
        return;
      }
      showFormError(null);
      if (wz.step < 3) setWizardStep(wz.step + 1);
      else await saveWizard();
    }
  });

  wz.win.addEventListener('action', (e) => {
    if (e.detail?.action === 'close') wz.cleanup();
  });

  wz.body.querySelector('[data-open-assist]')?.addEventListener('click', () => openAssist());

  wz.body.querySelectorAll('[data-persona]').forEach((card) => {
    card.addEventListener('click', () => applyPersona(card.dataset.persona));
    card.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        applyPersona(card.dataset.persona);
      }
    });
  });

  const slider = wz.body.querySelector('#agent-wz-max-iterations');
  const sliderVal = wz.body.querySelector('[data-iterations-val]');
  slider?.addEventListener('input', (e) => {
    if (sliderVal) sliderVal.textContent = String(e.detail?.value ?? slider.value);
  });

  // Poziomy rozumowania wynikają z modelu, więc przebudowują się przy każdej
  // zmianie wyboru — nie ma jednej stałej listy do pokazania.
  const modelSelect = wz.body.querySelector('#agent-wz-model');
  modelSelect?.addEventListener('change', () => refreshReasoningOptions(wz));
  refreshReasoningOptions(wz);

  renderToolPicker();
  renderWizardSkills();
}

async function refreshReasoningOptions(wz) {
  const field = wz.body.querySelector('#agent-wz-reasoning-field');
  const select = wz.body.querySelector('#agent-wz-reasoning');
  if (!field || !select) return;

  await ModelModalities.load();
  const model = (wz.body.querySelector('#agent-wz-model')?.value || '').trim();
  const levels = ModelModalities.reasoningLevels(model);
  const current = (select.value || '').trim();

  if (Array.isArray(levels) && levels.length === 0) {
    field.hidden = true;
    select.value = '';
    return;
  }

  const options = Array.isArray(levels) && levels.length
    ? levels
    : (current ? [current] : []);
  if (!options.length) {
    field.hidden = true;
    select.value = '';
    return;
  }

  field.hidden = false;
  // Pusta opcja = "nie ruszaj ustawienia modelu", nie "poziom zerowy".
  select.innerHTML = `<option value="">${escapeHtml(t('reasoning_effort_default'))}</option>`
    + options.map((lvl) => `<option value="${escapeAttr(lvl)}">${escapeHtml(lvl)}</option>`).join('');
  select.value = options.includes(current) ? current : '';
}

function setWizardStep(step) {
  const wz = state.wizard;
  if (!wz) return;
  wz.step = step;
  wz.body.querySelectorAll('[data-step-panel]').forEach((panel) => {
    panel.hidden = Number(panel.dataset.stepPanel) !== step;
  });
  wz.body.querySelectorAll('[data-step-pill]').forEach((pill) => {
    const n = Number(pill.dataset.stepPill);
    pill.classList.toggle('is-active', n === step);
    pill.classList.toggle('is-done', n < step);
  });
  wz.body.querySelectorAll('[data-step-line]').forEach((line) => {
    line.classList.toggle('is-done', Number(line.dataset.stepLine) < step);
  });
  const backBtn = wz.foot.querySelector('[data-action="back"]');
  if (backBtn) backBtn.style.visibility = step > 1 ? 'visible' : 'hidden';
  const nextBtn = wz.foot.querySelector('[data-action="next"]');
  if (nextBtn) {
    if (step < 3) {
      nextBtn.setAttribute('icon', 'chevron-right');
      nextBtn.setAttribute('label', t('wizard_next'));
    } else {
      nextBtn.setAttribute('icon', 'check');
      nextBtn.setAttribute('label', t('wizard_create_open'));
    }
  }
}

// Persona presets only overwrite text the operator has not customised: an
// empty field or the text of the previously applied persona.
function applyPersona(personaId) {
  const wz = state.wizard;
  if (!wz) return;
  const field = (sel) => wz.body.querySelector(sel);
  const prev = PERSONAS.find((p) => p.id === wz.activePersona);
  const next = PERSONAS.find((p) => p.id === personaId);
  if (!next) return;

  const descField = field('#agent-wz-description');
  const promptField = field('#agent-wz-system-prompt');
  const descOwnedByPrev = !descField.value.trim() || (prev && descField.value.trim() === t(`persona_${prev.id}_desc`));
  const promptOwnedByPrev = !promptField.value.trim() || (prev && promptField.value.trim() === t(`persona_${prev.id}_prompt`));

  if (descOwnedByPrev) descField.value = t(`persona_${next.id}_desc`);
  if (promptOwnedByPrev) promptField.value = t(`persona_${next.id}_prompt`);

  wz.body.querySelectorAll('[data-persona]').forEach((card) => {
    card.classList.toggle('is-selected', card.dataset.persona === personaId);
  });
  wz.activePersona = personaId;
}

function showFormError(message) {
  const wz = state.wizard;
  const box = wz?.body.querySelector('[data-form-error]');
  if (!box) return;
  box.textContent = message || '';
  box.hidden = !message;
}

function intField(sel, fallback) {
  const raw = state.wizard?.body.querySelector(sel)?.value;
  const n = Number.parseInt(raw, 10);
  return Number.isFinite(n) ? n : fallback;
}

function validateWizardStep(step) {
  const wz = state.wizard;
  if (!wz) return null;
  const field = (sel) => wz.body.querySelector(sel);
  if (step === 1) {
    const name = (field('#agent-wz-name')?.value || '').trim();
    if (!name) return t('err_name_required');
    if (name.length > NAME_MAX_CHARS) return t('err_name_length');
    if (!KEBAB_REGEX.test(name)) return t('err_name_format');
    if (state.agents.some((a) => a.name === name)) return t('err_name_taken');
  }
  return null;
}

function buildAgentParams(source, field) {
  const params = { ...(source || {}) };
  const tempRaw = field('#agent-wz-temperature')?.value;
  if (tempRaw === '' || tempRaw == null) delete params.temperature;
  else params.temperature = Number(tempRaw);
  const reasoning = (field('#agent-wz-reasoning')?.value || '').trim();
  if (!reasoning) delete params.reasoning_effort;
  else params.reasoning_effort = reasoning;
  return params;
}

async function saveWizard() {
  const wz = state.wizard;
  if (!wz) return;
  const field = (sel) => wz.body.querySelector(sel);

  const step1Error = validateWizardStep(1);
  if (step1Error) {
    setWizardStep(1);
    showFormError(step1Error);
    return;
  }

  const name = (field('#agent-wz-name')?.value || '').trim();
  const payload = {
    name,
    display_name: (field('#agent-wz-display-name')?.value || '').trim() || null,
    description: (field('#agent-wz-description')?.value || '').trim(),
    system_prompt: (field('#agent-wz-system-prompt')?.value || '').trim() || null,
    model: (field('#agent-wz-model')?.value || '').trim() || null,
    tools: [...wz.selectedTools],
    skills: {
      names: [...wz.selectedSkillNames],
      tags: [...wz.selectedSkillTags],
    },
    // Zachowujemy klucze, ktorych ten formularz nie zna (params_json jest
    // wspolnym workiem), a nadpisujemy tylko te dwa. Puste pole = USUNIECIE
    // klucza, nie zapis pustej wartosci — inaczej backend dostalby "" jako
    // poziom rozumowania.
    params: buildAgentParams(wz.sourceParams, field),
    max_iterations: Number.parseInt(field('#agent-wz-max-iterations')?.value, 10) || 25,
    timeout_secs: intField('#agent-wz-timeout-secs', 600),
    max_subagents: intField('#agent-wz-max-subagents', 0),
    max_spawn_depth: intField('#agent-wz-max-spawn-depth', 1),
    on_child_complete: (field('#agent-wz-on-child-complete')?.value || 'notify').trim(),
    flow_id: (field('#agent-wz-flow-id')?.value || '').trim() || null,
    routable: field('#agent-wz-routable')?.hasAttribute('checked') ?? true,
    is_enabled: field('#agent-wz-enabled')?.hasAttribute('checked') ?? true,
  };

  try {
    await ApiBinary.one('agentsUpsertRequest', { agentJson: JSON.stringify(payload) });
    toast(t('create_ok'), 'success');
    wz.cleanup();
    await loadAgents();
    // "Create and configure": land in the new agent's detail (Konfiguracja).
    const created = state.agents.find((a) => a.name === name);
    if (created) openDetail(created.id, 'config');
  } catch (err) {
    showFormError(err.message);
  }
}

// =============================================================================
// Wizard step 2 pickers (tool groups + skills) — same universe as the tools
// tab (catalog packages included, marked read-only until installed).
// =============================================================================

function renderToolPicker() {
  const wz = state.wizard;
  const host = wz?.body.querySelector('#agent-wz-tools');
  if (!host) return;
  const { addons, core } = wz.catalog;

  const instanceGroups = addons.filter((g) => g.installed !== false);
  const packageGroups = addons.filter((g) => g.installed === false);

  const groupsHtml = instanceGroups.map((group) => {
    const wildcard = `${group.addon_id}.*`;
    const wildcardOn = wz.selectedTools.has(wildcard);
    const { title, subtitle } = addonGroupLabel(group);
    return `
      <div class="agents-tool-group" data-group="${escapeAttr(group.addon_id)}">
        <div class="agents-tool-group-head" data-group-head role="button" tabindex="0">
          <div class="agents-tool-group-meta">
            <div class="agents-tool-group-title">
              <tf-chip status="accent">${escapeHtml(title)}</tf-chip>
              <span class="agents-tool-group-id" title="${escapeAttr(t('tools_group_instance_id', { id: group.addon_id }))}">${escapeHtml(group.addon_id)}</span>
            </div>
            <span class="agents-tool-group-sub" title="${escapeAttr(subtitle)}">${escapeHtml(subtitle)}</span>
          </div>
          <tf-toggle data-group-toggle="${escapeAttr(wildcard)}" title="${escapeAttr(t('tool_wildcard_hint'))}" ${wildcardOn ? 'checked' : ''}></tf-toggle>
          <span class="agents-tool-chev">${sprite('chevron-down')}</span>
        </div>
        <div class="agents-tool-group-body" hidden>
          ${group.tools.map((tool) => `
            <div class="agents-tool-item">
              <span class="agents-tool-name">${escapeHtml(tool.name)}</span>
              <span class="agents-tool-desc" title="${escapeAttr(tool.description || '')}">${escapeHtml(tool.description || '')}</span>
              <tf-toggle data-tool="${escapeAttr(tool.name)}" ${wz.selectedTools.has(tool.name) ? 'checked' : ''} ${wildcardOn ? 'disabled' : ''}></tf-toggle>
            </div>
          `).join('')}
        </div>
      </div>
    `;
  }).join('');

  const packageHtml = packageGroups.map((group) => {
    const { title, subtitle } = addonGroupLabel(group);
    return `
      <div class="agents-tool-group is-not-installed">
        <div class="agents-tool-group-head">
          <div class="agents-tool-group-meta">
            <div class="agents-tool-group-title">
              <tf-chip>${escapeHtml(title)}</tf-chip>
              <span class="ag-install-hint">${sprite('alert')} ${escapeHtml(t('tools_requires_install'))}</span>
            </div>
            <span class="agents-tool-group-sub" title="${escapeAttr(subtitle)}">${escapeHtml(subtitle)}</span>
          </div>
        </div>
      </div>
    `;
  }).join('');

  const coreHtml = core.length ? `
    <div class="agents-tool-group" data-group="core">
      <div class="agents-tool-group-head" data-group-head role="button" tabindex="0">
        <tf-chip status="info">${escapeHtml(t('tools_group_core'))}</tf-chip>
        <span class="agents-tool-group-sub">${escapeHtml(t('tools_core_sub'))}</span>
        <span class="agents-tool-chev">${sprite('chevron-down')}</span>
      </div>
      <div class="agents-tool-group-body" hidden>
        ${core.map((tool) => `
          <div class="agents-tool-item">
            <span class="agents-tool-name">${escapeHtml(tool.name)}</span>
            <span class="agents-tool-desc" title="${escapeAttr(tool.description || '')}">${escapeHtml(tool.description || '')}</span>
            <tf-toggle data-tool="${escapeAttr(tool.name)}" ${wz.selectedTools.has(tool.name) ? 'checked' : ''}></tf-toggle>
          </div>
        `).join('')}
      </div>
    </div>
  ` : '';

  const all = groupsHtml + packageHtml + coreHtml;
  host.innerHTML = all || `<div class="agents-tree-empty">${escapeHtml(t('tools_empty'))}</div>`;

  host.addEventListener('click', (e) => {
    if (e.target.closest('tf-toggle')) return;
    const head = e.target.closest('[data-group-head]');
    if (!head) return;
    const group = head.closest('.agents-tool-group');
    const body = group?.querySelector('.agents-tool-group-body');
    if (body) {
      body.hidden = !body.hidden;
      group.classList.toggle('is-open', !body.hidden);
    }
  });

  host.addEventListener('change', (e) => {
    const groupToggle = e.target.closest('tf-toggle[data-group-toggle]');
    if (groupToggle) {
      const wildcard = groupToggle.dataset.groupToggle;
      const checked = e.detail?.checked ?? groupToggle.hasAttribute('checked');
      if (checked) wz.selectedTools.add(wildcard);
      else wz.selectedTools.delete(wildcard);
      // A wildcard covers the whole addon — individual rows lock while it is on.
      const group = groupToggle.closest('.agents-tool-group');
      group?.querySelectorAll('tf-toggle[data-tool]').forEach((tg) => {
        if (checked) tg.setAttribute('disabled', '');
        else tg.removeAttribute('disabled');
      });
      return;
    }
    const toolToggle = e.target.closest('tf-toggle[data-tool]');
    if (!toolToggle) return;
    const name = toolToggle.dataset.tool;
    const checked = e.detail?.checked ?? toolToggle.hasAttribute('checked');
    if (checked) wz.selectedTools.add(name);
    else wz.selectedTools.delete(name);
  });
}

function renderWizardSkills() {
  const wz = state.wizard;
  const host = wz?.body.querySelector('#agent-wz-skills');
  if (!host) return;

  const tags = allWizardSkillTags();
  const skills = wz.skills;

  if (!skills.length && !tags.length) {
    host.innerHTML = `<div class="agents-tree-empty">${escapeHtml(t('skills_empty'))}</div>`;
    return;
  }

  const tagRows = tags.map((tag) => `
    <label class="agents-skill-row">
      <tf-checkbox data-skill-tag="${escapeAttr(tag)}" ${wz.selectedSkillTags.has(tag) ? 'checked' : ''}></tf-checkbox>
      <tf-chip status="info">#${escapeHtml(tag)}</tf-chip>
    </label>
  `).join('');

  const nameRows = skills.map((s) => `
    <label class="agents-skill-row">
      <tf-checkbox data-skill-name="${escapeAttr(s.name)}" ${wz.selectedSkillNames.has(s.name) ? 'checked' : ''}></tf-checkbox>
      <span class="agents-skill-name">${escapeHtml(s.name)}</span>
      <span class="agents-skill-desc" title="${escapeAttr(s.description || '')}">${escapeHtml(s.description || '')}</span>
    </label>
  `).join('');

  host.innerHTML = `
    ${tags.length ? `<div class="agents-skill-section">
      <div class="agents-field-hint">${escapeHtml(t('skills_by_tag'))}</div>
      ${tagRows}
    </div>` : ''}
    ${skills.length ? `<div class="agents-skill-section">
      <div class="agents-field-hint">${escapeHtml(t('skills_by_name'))}</div>
      ${nameRows}
    </div>` : ''}
  `;

  host.addEventListener('change', (e) => {
    const cb = e.target.closest('tf-checkbox[data-skill-name], tf-checkbox[data-skill-tag]');
    if (!cb) return;
    const checked = e.detail?.checked ?? cb.hasAttribute('checked');
    if (cb.dataset.skillName !== undefined) {
      if (checked) wz.selectedSkillNames.add(cb.dataset.skillName);
      else wz.selectedSkillNames.delete(cb.dataset.skillName);
    } else if (cb.dataset.skillTag !== undefined) {
      if (checked) wz.selectedSkillTags.add(cb.dataset.skillTag);
      else wz.selectedSkillTags.delete(cb.dataset.skillTag);
    }
  });
}

function allWizardSkillTags() {
  const set = new Set();
  for (const skill of state.wizard?.skills ?? []) {
    for (const tag of parseStringArray(skill.tags_json)) set.add(tag);
  }
  return [...set].sort();
}

// =============================================================================
// Builder assistant (A08 mockup) — chat window feeding the wizard OR the open
// detail draft, depending on where it was opened from.
// =============================================================================

function openAssist(target = 'wizard') {
  state.assist?.cleanup();
  sweepWindows('assist');

  const win = document.createElement('tf-window');
  win.setAttribute('title', t('assist_title'));
  win.setAttribute('subtitle', t('assist_sub_working'));
  win.setAttribute('icon', 'sparkle');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '680');
  win.setAttribute('draggable', '');

  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'agents-assist';
  body.innerHTML = `
    <div class="agents-pg-banner">
      ${sprite('message')}
      <span>${escapeHtml(t(target === 'detail' ? 'assist_banner_detail' : 'assist_banner'))}</span>
    </div>
    <div class="agents-assist-chat" data-assist-chat></div>
    <tf-chat-composer placeholder="${escapeAttr(t('assist_composer_placeholder'))}"></tf-chat-composer>
  `;

  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.className = 'agents-editor-footer';
  foot.innerHTML = `
    <div class="agents-footer-left">
      <tf-button variant="ghost" icon="refresh" data-assist-regenerate>${escapeHtml(t('assist_regenerate'))}</tf-button>
    </div>
    <div class="agents-footer-right">
      <tf-button variant="primary" icon="check" data-assist-insert disabled>${escapeHtml(t('assist_insert'))}</tf-button>
      <tf-button variant="ghost" data-assist-close>${escapeHtml(t('assist_close'))}</tf-button>
    </div>
  `;
  win.append(body, foot);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  win.setAttribute('data-agents-window', 'assist');
  backdrop.setAttribute('data-agents-window', 'assist');
  document.body.append(backdrop, win);

  const cleanup = () => {
    win.remove();
    backdrop.remove();
    if (state.assist?.win === win) state.assist = null;
  };
  win.addEventListener('close-request', () => cleanup());
  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'close') cleanup();
  });

  state.assist = {
    win, body, foot, target,
    messages: [],
    proposal: null,
    busy: false,
    cleanup,
  };

  body.querySelector('tf-chat-composer')?.addEventListener('send', (e) => {
    const text = String(e.detail?.text ?? '').trim();
    if (text) sendAssistMessage(text);
  });
  foot.querySelector('[data-assist-close]')?.addEventListener('click', cleanup);
  foot.querySelector('[data-assist-regenerate]')?.addEventListener('click', regenerateAssistProposal);
  foot.querySelector('[data-assist-insert]')?.addEventListener('click', insertAssistProposal);

  renderAssist();
}

function renderAssist() {
  const as = state.assist;
  if (!as) return;
  const chat = as.body.querySelector('[data-assist-chat]');
  if (!chat) return;
  chat.innerHTML = as.messages.map((m) => `
    <tf-chat-bubble role="${m.role === 'user' ? 'user' : 'assistant'}"
      sender="${escapeAttr(m.role === 'user' ? '' : t('assist_sender'))}">${escapeHtml(m.content)}</tf-chat-bubble>
  `).join('');
  if (as.proposal) chat.appendChild(assistProposalHtml(as.proposal));
  chat.scrollTop = chat.scrollHeight;
  const insertBtn = as.foot.querySelector('[data-assist-insert]');
  if (insertBtn) {
    if (as.proposal) insertBtn.removeAttribute('disabled');
    else insertBtn.setAttribute('disabled', '');
  }
  const composer = as.body.querySelector('tf-chat-composer');
  if (composer) {
    if (as.busy) composer.setAttribute('disabled', '');
    else composer.removeAttribute('disabled');
  }
}

function assistProposalHtml(proposal) {
  const tools = Array.isArray(proposal.tools) ? proposal.tools : [];
  return `
    <div class="agents-assist-proposal">
      <div class="agents-assist-proposal-title">${sprite('sparkle')} ${escapeHtml(t('assist_proposal_title'))}</div>
      ${proposal.name ? `<div><strong>${escapeHtml(t('label_name'))}:</strong> <span class="mono">${escapeHtml(proposal.name)}</span></div>` : ''}
      ${proposal.display_name ? `<div><strong>${escapeHtml(t('label_display_name'))}:</strong> ${escapeHtml(proposal.display_name)}</div>` : ''}
      ${proposal.description ? `<div><strong>${escapeHtml(t('label_description'))}:</strong> ${escapeHtml(proposal.description)}</div>` : ''}
      ${proposal.system_prompt ? `<pre class="mono">${escapeHtml(proposal.system_prompt)}</pre>` : ''}
      ${tools.length ? `<div><strong>${escapeHtml(t('tools_section'))}:</strong> ${escapeHtml(tools.join(', '))}</div>` : ''}
    </div>
  `;
}

async function sendAssistMessage(text) {
  const as = state.assist;
  if (!as || as.busy) return;
  as.messages.push({ role: 'user', content: text });
  await requestAssistTurn();
}

// Re-asks the backend with the transcript trimmed back to the last user
// message, so a regenerate produces a fresh reply for the same context.
async function regenerateAssistProposal() {
  const as = state.assist;
  if (!as || as.busy) return;
  while (as.messages.length && as.messages[as.messages.length - 1].role === 'assistant') {
    as.messages.pop();
  }
  as.proposal = null;
  await requestAssistTurn();
}

async function requestAssistTurn() {
  const as = state.assist;
  if (!as) return;
  as.busy = true;
  renderAssist();
  try {
    const resp = await ApiBinary.one('agentBuilderAssistRequest', {
      messagesJson: JSON.stringify(as.messages),
    });
    const result = JSON.parse(resp.resultJson ?? resp.result_json ?? '{}');
    if (state.assist !== as) return;
    if (result.reply) as.messages.push({ role: 'assistant', content: String(result.reply) });
    as.proposal = result.proposal && typeof result.proposal === 'object' ? result.proposal : null;
  } catch (err) {
    if (state.assist !== as) return;
    toast(`${t('assist_failed')}: ${err.message}`, 'error');
  } finally {
    if (state.assist === as) {
      as.busy = false;
      renderAssist();
    }
  }
}

function kebabize(value) {
  return String(value || '')
    .toLowerCase()
    .normalize('NFD')
    .replace(/[\u0300-\u036f]/g, '')
    .replace(/ł/g, 'l')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, NAME_MAX_CHARS)
    .replace(/-+$/g, '');
}

function insertAssistProposal() {
  const as = state.assist;
  if (!as?.proposal) return;
  if (as.target === 'detail') insertProposalIntoDetail(as);
  else insertProposalIntoWizard(as);
  toast(t('assist_inserted'), 'success');
  as.cleanup();
}

// Only entries that exist in the local catalog (or their addon.* wildcards)
// enter the allowlist — the assistant model may hallucinate tool names.
function filterKnownTools(rawTools, catalog) {
  const known = new Set();
  for (const group of catalog.addons) {
    known.add(`${group.addon_id}.*`);
    for (const tool of group.tools) known.add(tool.name);
  }
  for (const tool of catalog.core) known.add(tool.name);
  const out = [];
  for (const name of Array.isArray(rawTools) ? rawTools : []) {
    if (typeof name === 'string' && known.has(name)) out.push(name);
  }
  return out;
}

function insertProposalIntoWizard(as) {
  const wz = state.wizard;
  const p = as.proposal;
  if (!wz) return;
  const field = (sel) => wz.body.querySelector(sel);

  const nameField = field('#agent-wz-name');
  const proposedName = kebabize(p.name || p.display_name);
  if (nameField && proposedName && !nameField.value.trim()) nameField.value = proposedName;
  const displayField = field('#agent-wz-display-name');
  if (displayField && p.display_name) displayField.value = String(p.display_name);
  const descField = field('#agent-wz-description');
  if (descField && p.description) descField.value = String(p.description);
  const promptField = field('#agent-wz-system-prompt');
  if (promptField && p.system_prompt) promptField.value = String(p.system_prompt);

  const proposedIterations = Number.parseInt(p.max_iterations, 10);
  if (Number.isFinite(proposedIterations) && proposedIterations >= 1) {
    const slider = field('#agent-wz-max-iterations');
    const capped = Math.min(proposedIterations, MAX_ITERATIONS_CAP);
    if (slider) slider.value = String(capped);
    const sliderVal = wz.body.querySelector('[data-iterations-val]');
    if (sliderVal) sliderVal.textContent = String(capped);
  }

  if (Array.isArray(p.tools) && p.tools.length) {
    for (const name of filterKnownTools(p.tools, wz.catalog)) wz.selectedTools.add(name);
    renderToolPicker();
  }
}

function insertProposalIntoDetail(as) {
  const d = state.detail;
  const p = as.proposal;
  if (!d) return;
  if (p.display_name) d.cfg.display_name = String(p.display_name);
  if (p.description) d.cfg.description = String(p.description);
  if (p.system_prompt) d.cfg.system_prompt = String(p.system_prompt);
  if (Array.isArray(p.tools) && p.tools.length) {
    for (const name of filterKnownTools(p.tools, d.catalog)) d.selectedTools.add(name);
  }
  markDirty();
  // Re-render the tab the operator came from so inserted values are visible.
  switchDetailTab(d.tab === 'tools' ? 'tools' : d.tab);
}

// =============================================================================
// Team templates (A09 mockup)
// =============================================================================

function openTemplates() {
  state.templatesWin?.cleanup();
  sweepWindows('templates');

  const win = document.createElement('tf-window');
  win.setAttribute('title', t('tpl_title'));
  win.setAttribute('icon', 'users');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '760');
  win.setAttribute('draggable', '');

  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'agents-templates';
  body.innerHTML = `
    <div class="agents-field-hint">${escapeHtml(t('tpl_intro'))}</div>
    <div class="agents-templates-head">${escapeHtml(t('tpl_ready'))} <tf-chip status="info">${TEAM_TEMPLATES.length}</tf-chip></div>
    ${TEAM_TEMPLATES.map(templateCardHtml).join('')}
  `;
  win.appendChild(body);

  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.className = 'agents-editor-footer';
  foot.innerHTML = `
    <div class="agents-footer-left">
      <tf-button variant="ghost" icon="plus" data-tpl-action="own">${escapeHtml(t('tpl_own_agent'))}</tf-button>
    </div>
    <div class="agents-footer-right">
      <tf-button variant="ghost" data-tpl-action="close">${escapeHtml(t('tpl_close'))}</tf-button>
    </div>
  `;
  win.appendChild(foot);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  win.setAttribute('data-agents-window', 'templates');
  backdrop.setAttribute('data-agents-window', 'templates');
  document.body.append(backdrop, win);

  const cleanup = () => {
    win.remove();
    backdrop.remove();
    if (state.templatesWin?.win === win) state.templatesWin = null;
  };
  win.addEventListener('close-request', () => cleanup());
  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'close') cleanup();
  });

  state.templatesWin = { win, cleanup };

  body.addEventListener('click', (e) => {
    const createBtn = e.target.closest('[data-tpl-create]');
    if (createBtn) createTeam(createBtn.dataset.tplCreate, createBtn);
  });
  foot.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-tpl-action]');
    if (!btn) return;
    if (btn.dataset.tplAction === 'close') cleanup();
    else if (btn.dataset.tplAction === 'own') {
      cleanup();
      openWizard(null);
    }
  });
}

function templateCardHtml(tpl) {
  const flow = tpl.flow.map((node, i) => `
    ${i ? `<span class="agents-tpl-arrow">${sprite('chevron-right')}</span>` : ''}
    <span class="agents-tpl-node">
      <span class="agents-tpl-node-ico">${sprite(node.icon)}</span>
      <span class="agents-tpl-node-label">${escapeHtml(t(node.labelKey))}</span>
    </span>
  `).join('');
  const agents = tpl.agents.map((a) => `
    <div class="agents-tpl-agent">
      <span class="agents-tpl-agent-ico">${sprite(a.icon)}</span>
      <span><strong>${escapeHtml(t(a.displayKey))}</strong> — ${escapeHtml(t(a.roleKey))}</span>
    </div>
  `).join('');
  return `
    <div class="agents-tpl-card">
      <div class="agents-tpl-ico">${sprite(tpl.icon)}</div>
      <div class="agents-tpl-main">
        <div class="agents-tpl-name">${escapeHtml(t(tpl.nameKey))}</div>
        <div class="agents-tpl-desc">${escapeHtml(t(tpl.descKey))}</div>
        <div class="agents-tpl-flow">
          ${flow}
          ${tpl.loop ? `<tf-chip status="info">${escapeHtml(t('tpl_loop_chip'))}</tf-chip>` : ''}
        </div>
        <div class="agents-tpl-agents">${agents}</div>
        <div class="agents-tpl-foot">
          <tf-chip status="accent">${escapeHtml(t('tpl_agents_count', { count: tpl.agents.length }))}</tf-chip>
          <tf-button variant="primary" size="sm" icon="plus" data-tpl-create="${escapeAttr(tpl.id)}">${escapeHtml(t('tpl_create'))}</tf-button>
        </div>
      </div>
    </div>
  `;
}

async function createTeam(templateId, btn) {
  const tpl = TEAM_TEMPLATES.find((x) => x.id === templateId);
  if (!tpl) return;

  const existing = new Set(state.agents.map((a) => a.name));
  const conflicts = tpl.agents.filter((a) => existing.has(a.name)).map((a) => a.name);
  if (conflicts.length) {
    toast(t('tpl_conflict', { names: conflicts.join(', ') }), 'error');
    return;
  }

  btn?.setAttribute('disabled', '');
  try {
    for (const def of tpl.agents) {
      const payload = {
        name: def.name,
        display_name: t(def.displayKey),
        description: t(def.roleKey),
        system_prompt: t(def.promptKey),
        model: null,
        tools: [],
        skills: { names: [], tags: [] },
        params: {},
        max_iterations: 25,
        timeout_secs: 600,
        max_subagents: def.maxSubagents ?? 0,
        max_spawn_depth: def.maxSpawnDepth ?? 1,
        on_child_complete: 'notify',
        flow_id: null,
        routable: true,
        is_enabled: true,
      };
      await ApiBinary.one('agentsUpsertRequest', { agentJson: JSON.stringify(payload) });
    }
    toast(t('tpl_created', { count: tpl.agents.length }), 'success');
    state.templatesWin?.cleanup();
    await loadAgents();
  } catch (err) {
    toast(`${t('tpl_create_failed')}: ${err.message}`, 'error');
    btn?.removeAttribute('disabled');
  }
}
