// ===== File: agents.js — Agents registry admin screen (Harness plan §3.3) =====
//
// List + CRUD UI for the `agents` table plus a read-only "Runs" view over the
// runtime `agent_runs` rows — all over the binary protocol
// (`MessageBody::AgentsBody` via ApiBinary + stage-D codec helpers, never REST).
// The editor pickers: a model tf-combobox (dynamic models list), a tool tree
// (tf-checkbox grouped by addon with an addon.* wildcard row + core.* builtins)
// from ToolsCatalogRequest, a skill picker by name+tag from SkillsListRequest,
// numeric limits, and routable/enabled tf-toggles. tf-* components only.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
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
import '/js/components/tf-empty-state.js';

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

const state = {
  agents: [],
  searchQuery: '',
  enabledFilter: 'all',
  routableFilter: 'all',
  editor: null,
};

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

const AgentsScreen = {
  get title() { return I18n.t('agents.title'); },

  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('brain')} ${escapeHtml(I18n.t('agents.title'))}</h1>
          <div class="sub" id="agents-sub"></div>
        </div>
        <div class="actions">
          <tf-button variant="ghost" icon="refresh" id="agents-refresh">${escapeHtml(I18n.t('agents.refresh'))}</tf-button>
          <tf-button variant="primary" icon="plus" id="agents-new">${escapeHtml(I18n.t('agents.new_agent'))}</tf-button>
        </div>
      </div>

      <section class="card agents-card">
        <div class="agents-toolbar">
          <tf-searchbox id="agents-search" placeholder="${escapeAttr(I18n.t('agents.search_placeholder'))}" debounce="200"></tf-searchbox>
          <tf-select id="agents-filter-enabled" class="agents-filter" value="all">
            <option value="all">${escapeHtml(I18n.t('agents.filter_enabled_all'))}</option>
            <option value="enabled">${escapeHtml(I18n.t('agents.filter_enabled_on'))}</option>
            <option value="disabled">${escapeHtml(I18n.t('agents.filter_enabled_off'))}</option>
          </tf-select>
          <tf-select id="agents-filter-routable" class="agents-filter" value="all">
            <option value="all">${escapeHtml(I18n.t('agents.filter_routable_all'))}</option>
            <option value="routable">${escapeHtml(I18n.t('agents.filter_routable_on'))}</option>
            <option value="not_routable">${escapeHtml(I18n.t('agents.filter_routable_off'))}</option>
          </tf-select>
        </div>
        <div id="agents-table-host" class="agents-table-host"></div>
      </section>
    `;
  },

  async mount() {
    byId('agents-refresh')?.addEventListener('click', () => loadAgents());
    byId('agents-new')?.addEventListener('click', () => openAgentEditor(null));
    byId('agents-search')?.addEventListener('search', (e) => {
      state.searchQuery = String(e.detail?.value ?? '');
      renderTable();
    });
    byId('agents-filter-enabled')?.addEventListener('change', (e) => {
      state.enabledFilter = e.detail?.value ?? e.target.value ?? 'all';
      renderTable();
    });
    byId('agents-filter-routable')?.addEventListener('change', (e) => {
      state.routableFilter = e.detail?.value ?? e.target.value ?? 'all';
      renderTable();
    });
    await loadAgents();
  },

  unmount() {
    state.editor?.cleanup();
    state.agents = [];
    state.searchQuery = '';
    state.enabledFilter = 'all';
    state.routableFilter = 'all';
  },
};

export default AgentsScreen;

// =============================================================================
// Data
// =============================================================================

async function loadAgents() {
  try {
    const resp = await ApiBinary.one('agentsListRequest', {});
    const rows = JSON.parse(resp.agentsJson ?? resp.agents_json ?? '[]');
    state.agents = Array.isArray(rows) ? rows : [];
    const sub = byId('agents-sub');
    if (sub) sub.textContent = I18n.t('agents.subtitle', { count: state.agents.length });
    renderTable();
  } catch (err) {
    toast(`${I18n.t('agents.load_failed')}: ${err.message}`, 'error');
  }
}

function parseStringArray(json) {
  try {
    const arr = JSON.parse(json || '[]');
    return Array.isArray(arr) ? arr.filter((t) => typeof t === 'string') : [];
  } catch {
    return [];
  }
}

function parseSkillsSelection(json) {
  try {
    const obj = JSON.parse(json || '{}');
    const names = Array.isArray(obj?.names) ? obj.names.filter((s) => typeof s === 'string') : [];
    const tags = Array.isArray(obj?.tags) ? obj.tags.filter((s) => typeof s === 'string') : [];
    return { names, tags };
  } catch {
    return { names: [], tags: [] };
  }
}

function filteredAgents() {
  const query = state.searchQuery.trim().toLowerCase();
  return state.agents.filter((agent) => {
    if (state.enabledFilter === 'enabled' && !agent.is_enabled) return false;
    if (state.enabledFilter === 'disabled' && agent.is_enabled) return false;
    if (state.routableFilter === 'routable' && !agent.routable) return false;
    if (state.routableFilter === 'not_routable' && agent.routable) return false;
    if (query) {
      const haystack = [agent.name, agent.display_name || '', agent.description || '', agent.model || '']
        .join(' ')
        .toLowerCase();
      if (!haystack.includes(query)) return false;
    }
    return true;
  });
}

// =============================================================================
// List rendering
// =============================================================================

function renderTable() {
  const host = byId('agents-table-host');
  if (!host) return;
  const visible = filteredAgents();

  if (!visible.length) {
    const noAgentsAtAll = state.agents.length === 0;
    host.innerHTML = `
      <tf-empty-state icon="brain"
        title="${escapeAttr(I18n.t(noAgentsAtAll ? 'agents.empty_list' : 'agents.empty_match'))}"
        message="${escapeAttr(noAgentsAtAll ? I18n.t('agents.empty_list_hint') : '')}"></tf-empty-state>
    `;
    return;
  }

  host.innerHTML = `
    <tf-table id="agents-table" sortable>
      <tf-column key="name" label="${escapeAttr(I18n.t('agents.col_name'))}" sortable></tf-column>
      <tf-column key="model" label="${escapeAttr(I18n.t('agents.col_model'))}" sortable></tf-column>
      <tf-column key="tools" label="${escapeAttr(I18n.t('agents.col_tools'))}" renderer="num" sortable></tf-column>
      <tf-column key="skills" label="${escapeAttr(I18n.t('agents.col_skills'))}" renderer="num" sortable></tf-column>
      <tf-column key="routable" label="${escapeAttr(I18n.t('agents.col_routable'))}" renderer="chip"></tf-column>
      <tf-column key="enabled" label="${escapeAttr(I18n.t('agents.col_enabled'))}" renderer="chip"></tf-column>
    </tf-table>
  `;

  const table = byId('agents-table');
  table.rows = visible.map(agentToRow);
  table.rowActions = buildRowActions;
  table.addEventListener('row-click', (e) => {
    const id = e.detail?.row?._id;
    if (id) openAgentEditor(id);
  });
}

function countSkills(agent) {
  const sel = parseSkillsSelection(agent.skills_json);
  return sel.names.length + sel.tags.length;
}

function agentToRow(agent) {
  return {
    _id: agent.id,
    name: agent.display_name ? `${agent.name} · ${agent.display_name}` : agent.name,
    model: agent.model || I18n.t('agents.model_inherited'),
    tools: parseStringArray(agent.tools_json).length,
    skills: countSkills(agent),
    routable: {
      status: agent.routable ? 'ok' : 'info',
      label: I18n.t(agent.routable ? 'agents.routable_yes' : 'agents.routable_no'),
    },
    enabled: {
      status: agent.is_enabled ? 'ok' : 'warn',
      label: I18n.t(agent.is_enabled ? 'agents.enabled_yes' : 'agents.enabled_no'),
    },
  };
}

function buildRowActions(row) {
  const wrap = document.createElement('div');
  wrap.style.display = 'flex';
  wrap.style.gap = '4px';
  wrap.style.justifyContent = 'flex-end';

  const edit = document.createElement('tf-button');
  edit.setAttribute('variant', 'ghost');
  edit.setAttribute('size', 'sm');
  edit.textContent = I18n.t('agents.action_edit');
  edit.addEventListener('click', () => openAgentEditor(row._id));
  wrap.appendChild(edit);

  const del = document.createElement('tf-button');
  del.setAttribute('variant', 'danger');
  del.setAttribute('size', 'sm');
  del.textContent = I18n.t('agents.action_delete');
  del.addEventListener('click', () => deleteAgent(row._id));
  wrap.appendChild(del);

  return wrap;
}

function formatTimestamp(value) {
  if (!value) return '—';
  // SQLite datetime('now') yields "YYYY-MM-DD HH:MM:SS" in UTC, no zone marker.
  const iso = value.includes('T') ? value : `${value.replace(' ', 'T')}Z`;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(I18n.getLanguage());
}

// =============================================================================
// Delete
// =============================================================================

async function deleteAgent(agentId) {
  const agent = state.agents.find((a) => a.id === agentId);
  if (!agent) return;
  const ok = await TfWindow.confirm({
    title: I18n.t('agents.delete_confirm_title'),
    message: I18n.t('agents.delete_confirm_message', { name: escapeHtml(agent.name) }),
    confirmLabel: I18n.t('agents.action_delete'),
    cancelLabel: I18n.t('agents.action_cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await ApiBinary.one('agentsDeleteRequest', { agentId });
    toast(I18n.t('agents.delete_ok'), 'success');
    await loadAgents();
  } catch (err) {
    toast(`${I18n.t('agents.delete_failed')}: ${err.message}`, 'error');
  }
}

// =============================================================================
// Editor (tf-window with Settings + Runs tabs)
// =============================================================================

async function openAgentEditor(agentId) {
  state.editor?.cleanup();

  let agent = null;
  if (agentId) {
    try {
      const resp = await ApiBinary.one('agentsDetailRequest', { agentId });
      agent = JSON.parse(resp.agentJson ?? resp.agent_json ?? 'null');
    } catch (err) {
      toast(`${I18n.t('agents.detail_failed')}: ${err.message}`, 'error');
      return;
    }
    if (!agent) return;
  }

  // The catalog (pickable tools) and the skill list feed the pickers; both are
  // best-effort — a missing addon manager or skill table must not block editing.
  const [catalog, skills, models] = await Promise.all([
    loadToolsCatalog(),
    loadSkillList(),
    loadModels(),
  ]);

  const mode = agent ? 'edit' : 'create';
  const selectedTools = agent ? parseStringArray(agent.tools_json) : [];
  const skillSel = agent ? parseSkillsSelection(agent.skills_json) : { names: [], tags: [] };

  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t(mode === 'create' ? 'agents.editor_create_title' : 'agents.editor_edit_title'));
  if (agent) win.setAttribute('subtitle', agent.name);
  win.setAttribute('icon', 'brain');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '900');
  win.setAttribute('draggable', '');

  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'agents-editor';
  body.innerHTML = editorBodyHtml(agent, mode, models);
  win.appendChild(body);

  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.className = 'agents-editor-footer';
  foot.innerHTML = editorFooterHtml(mode);
  win.appendChild(foot);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);

  const cleanup = () => {
    if (win.isConnected) win.close(true);
    if (backdrop.isConnected) backdrop.remove();
    if (state.editor?.win === win) state.editor = null;
  };
  win.addEventListener('close-request', () => {
    if (backdrop.isConnected) backdrop.remove();
    if (state.editor?.win === win) state.editor = null;
  });

  state.editor = {
    mode,
    agent,
    win,
    body,
    catalog,
    skills,
    selectedTools: new Set(selectedTools),
    selectedSkillNames: new Set(skillSel.names),
    selectedSkillTags: new Set(skillSel.tags),
    runsLoaded: false,
    cleanup,
  };

  // Long strings are injected via the value property so they never round-trip
  // through escapeAttr + innerHTML parsing.
  const description = body.querySelector('#agent-ed-description');
  if (description) description.value = agent?.description || '';
  const systemPrompt = body.querySelector('#agent-ed-system-prompt');
  if (systemPrompt) systemPrompt.value = agent?.system_prompt || '';

  renderToolPicker();
  renderSkillPicker();
  wireEditor(foot);
  switchEditorTab('settings');
}

function editorBodyHtml(agent, mode, models) {
  const modelOptions = [
    `<option value="">${escapeHtml(I18n.t('agents.model_inherited'))}</option>`,
    ...models.map((m) => `<option value="${escapeAttr(m.value)}">${escapeHtml(m.label)}</option>`),
  ].join('');
  const runsTab = mode === 'edit'
    ? `<tf-tab id="runs" icon="clock">${escapeHtml(I18n.t('agents.tab_runs'))}</tf-tab>`
    : '';

  return `
    <tf-tabs variant="underline" value="settings" id="agent-ed-tabs">
      <tf-tab id="settings" icon="settings">${escapeHtml(I18n.t('agents.tab_settings'))}</tf-tab>
      ${runsTab}
    </tf-tabs>

    <div class="agents-tab-panel" data-panel="settings">
      <div class="agents-editor-grid">
        <tf-input id="agent-ed-name" label="${escapeAttr(I18n.t('agents.label_name'))}"
          value="${escapeAttr(agent?.name || '')}"
          hint="${escapeAttr(I18n.t('agents.hint_name'))}"
          maxlength="${NAME_MAX_CHARS}"></tf-input>
        <tf-input id="agent-ed-display-name" label="${escapeAttr(I18n.t('agents.label_display_name'))}"
          value="${escapeAttr(agent?.display_name || '')}"></tf-input>
      </div>

      <tf-textarea id="agent-ed-description" label="${escapeAttr(I18n.t('agents.label_description'))}"
        rows="3" hint="${escapeAttr(I18n.t('agents.hint_description'))}"></tf-textarea>

      <tf-textarea id="agent-ed-system-prompt" label="${escapeAttr(I18n.t('agents.label_system_prompt'))}"
        rows="6"></tf-textarea>

      <div class="agents-editor-grid">
        <tf-select id="agent-ed-model" label="${escapeAttr(I18n.t('agents.label_model'))}"
          value="${escapeAttr(agent?.model || '')}">${modelOptions}</tf-select>
        <tf-input id="agent-ed-flow-id" label="${escapeAttr(I18n.t('agents.label_flow_id'))}"
          value="${escapeAttr(agent?.flow_id || '')}"
          hint="${escapeAttr(I18n.t('agents.hint_flow_id'))}"></tf-input>
      </div>

      <div class="agents-field">
        <span class="tf-label">${escapeHtml(I18n.t('agents.label_tools'))}</span>
        <div class="agents-field-hint">${escapeHtml(I18n.t('agents.hint_tools'))}</div>
        <div id="agent-ed-tools" class="agents-tool-tree"></div>
      </div>

      <div class="agents-field">
        <span class="tf-label">${escapeHtml(I18n.t('agents.label_skills'))}</span>
        <div class="agents-field-hint">${escapeHtml(I18n.t('agents.hint_skills'))}</div>
        <div id="agent-ed-skills" class="agents-skill-tree"></div>
      </div>

      <div class="agents-editor-grid agents-limits">
        <tf-input id="agent-ed-max-iterations" type="number" label="${escapeAttr(I18n.t('agents.label_max_iterations'))}"
          value="${escapeAttr(String(agent?.max_iterations ?? 25))}" min="1" max="${MAX_ITERATIONS_CAP}"></tf-input>
        <tf-input id="agent-ed-timeout-secs" type="number" label="${escapeAttr(I18n.t('agents.label_timeout_secs'))}"
          value="${escapeAttr(String(agent?.timeout_secs ?? 600))}" min="1"></tf-input>
        <tf-input id="agent-ed-max-subagents" type="number" label="${escapeAttr(I18n.t('agents.label_max_subagents'))}"
          value="${escapeAttr(String(agent?.max_subagents ?? 0))}" min="0"></tf-input>
        <tf-input id="agent-ed-max-spawn-depth" type="number" label="${escapeAttr(I18n.t('agents.label_max_spawn_depth'))}"
          value="${escapeAttr(String(agent?.max_spawn_depth ?? 1))}" min="1"></tf-input>
      </div>

      <div class="agents-toggles">
        <label class="agents-toggle-row">
          <tf-toggle id="agent-ed-routable" ${agent ? (agent.routable ? 'checked' : '') : 'checked'}></tf-toggle>
          <span>${escapeHtml(I18n.t('agents.label_routable'))}</span>
        </label>
        <label class="agents-toggle-row">
          <tf-toggle id="agent-ed-enabled" ${agent ? (agent.is_enabled ? 'checked' : '') : 'checked'}></tf-toggle>
          <span>${escapeHtml(I18n.t('agents.label_enabled'))}</span>
        </label>
      </div>

      <div class="agents-form-error" data-form-error hidden></div>
    </div>

    <div class="agents-tab-panel" data-panel="runs" hidden>
      <div id="agent-ed-runs-host" class="agents-runs-host"></div>
    </div>
  `;
}

function editorFooterHtml(mode) {
  const saveLabel = I18n.t(mode === 'create' ? 'agents.action_create' : 'agents.action_save');
  return `
    <div class="agents-footer-left"></div>
    <div class="agents-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('agents.action_cancel'))}</tf-button>
      <tf-button variant="primary" data-action="save">${escapeHtml(saveLabel)}</tf-button>
    </div>
  `;
}

function switchEditorTab(tabId) {
  const ed = state.editor;
  if (!ed) return;
  ed.body.querySelectorAll('.agents-tab-panel').forEach((panel) => {
    panel.hidden = panel.dataset.panel !== tabId;
  });
  // The footer save button only applies to the Settings tab.
  const foot = ed.win.querySelector('.agents-editor-footer');
  if (foot) foot.style.visibility = tabId === 'settings' ? 'visible' : 'hidden';
  if (tabId === 'runs' && !ed.runsLoaded) {
    ed.runsLoaded = true;
    loadEditorRuns();
  }
}

// =============================================================================
// Tool picker (addon groups + addon.* wildcard + core.*)
// =============================================================================

function renderToolPicker() {
  const ed = state.editor;
  const host = ed?.body.querySelector('#agent-ed-tools');
  if (!host) return;
  const { addons, core } = ed.catalog;

  if (!addons.length && !core.length) {
    host.innerHTML = `<div class="agents-tree-empty">${escapeHtml(I18n.t('agents.tools_empty'))}</div>`;
    return;
  }

  const groupHtml = (group) => {
    const wildcard = `${group.addon_id}.*`;
    const toolRows = group.tools.map((t) => `
      <label class="agents-tool-row">
        <tf-checkbox data-tool="${escapeAttr(t.name)}" ${ed.selectedTools.has(t.name) ? 'checked' : ''}></tf-checkbox>
        <span class="agents-tool-name">${escapeHtml(t.name)}</span>
        <span class="agents-tool-desc" title="${escapeAttr(t.description)}">${escapeHtml(t.description)}</span>
      </label>
    `).join('');
    return `
      <div class="agents-tool-group">
        <div class="agents-tool-group-head">
          <tf-chip status="accent">${escapeHtml(group.addon_id)}</tf-chip>
          <label class="agents-tool-row agents-tool-wildcard">
            <tf-checkbox data-tool="${escapeAttr(wildcard)}" ${ed.selectedTools.has(wildcard) ? 'checked' : ''}></tf-checkbox>
            <span class="agents-tool-name">${escapeHtml(wildcard)}</span>
            <span class="agents-tool-desc">${escapeHtml(I18n.t('agents.tool_wildcard_hint'))}</span>
          </label>
        </div>
        ${toolRows}
      </div>
    `;
  };

  const coreHtml = core.length ? `
    <div class="agents-tool-group">
      <div class="agents-tool-group-head">
        <tf-chip status="info">core</tf-chip>
      </div>
      ${core.map((t) => `
        <label class="agents-tool-row">
          <tf-checkbox data-tool="${escapeAttr(t.name)}" ${ed.selectedTools.has(t.name) ? 'checked' : ''}></tf-checkbox>
          <span class="agents-tool-name">${escapeHtml(t.name)}</span>
          <span class="agents-tool-desc" title="${escapeAttr(t.description)}">${escapeHtml(t.description)}</span>
        </label>
      `).join('')}
    </div>
  ` : '';

  host.innerHTML = addons.map(groupHtml).join('') + coreHtml;

  host.addEventListener('change', (e) => {
    const cb = e.target.closest('tf-checkbox[data-tool]');
    if (!cb) return;
    const name = cb.dataset.tool;
    const checked = e.detail?.checked ?? cb.hasAttribute('checked');
    if (checked) ed.selectedTools.add(name);
    else ed.selectedTools.delete(name);
  });
}

// =============================================================================
// Skill picker (by name + by tag)
// =============================================================================

function allSkillTags() {
  const set = new Set();
  for (const skill of state.editor?.skills ?? []) {
    for (const tag of parseStringArray(skill.tags_json)) set.add(tag);
  }
  return [...set].sort();
}

function renderSkillPicker() {
  const ed = state.editor;
  const host = ed?.body.querySelector('#agent-ed-skills');
  if (!host) return;

  const tags = allSkillTags();
  const skills = ed.skills;

  if (!skills.length && !tags.length) {
    host.innerHTML = `<div class="agents-tree-empty">${escapeHtml(I18n.t('agents.skills_empty'))}</div>`;
    return;
  }

  const tagRows = tags.map((tag) => `
    <label class="agents-skill-row">
      <tf-checkbox data-skill-tag="${escapeAttr(tag)}" ${ed.selectedSkillTags.has(tag) ? 'checked' : ''}></tf-checkbox>
      <tf-chip status="info">#${escapeHtml(tag)}</tf-chip>
    </label>
  `).join('');

  const nameRows = skills.map((s) => `
    <label class="agents-skill-row">
      <tf-checkbox data-skill-name="${escapeAttr(s.name)}" ${ed.selectedSkillNames.has(s.name) ? 'checked' : ''}></tf-checkbox>
      <span class="agents-skill-name">${escapeHtml(s.name)}</span>
      <span class="agents-skill-desc" title="${escapeAttr(s.description || '')}">${escapeHtml(s.description || '')}</span>
    </label>
  `).join('');

  host.innerHTML = `
    ${tags.length ? `<div class="agents-skill-section">
      <div class="agents-field-hint">${escapeHtml(I18n.t('agents.skills_by_tag'))}</div>
      ${tagRows}
    </div>` : ''}
    ${skills.length ? `<div class="agents-skill-section">
      <div class="agents-field-hint">${escapeHtml(I18n.t('agents.skills_by_name'))}</div>
      ${nameRows}
    </div>` : ''}
  `;

  host.addEventListener('change', (e) => {
    const cb = e.target.closest('tf-checkbox[data-skill-name], tf-checkbox[data-skill-tag]');
    if (!cb) return;
    const checked = e.detail?.checked ?? cb.hasAttribute('checked');
    if (cb.dataset.skillName !== undefined) {
      if (checked) ed.selectedSkillNames.add(cb.dataset.skillName);
      else ed.selectedSkillNames.delete(cb.dataset.skillName);
    } else if (cb.dataset.skillTag !== undefined) {
      if (checked) ed.selectedSkillTags.add(cb.dataset.skillTag);
      else ed.selectedSkillTags.delete(cb.dataset.skillTag);
    }
  });
}

// =============================================================================
// Runs tab
// =============================================================================

async function loadEditorRuns() {
  const ed = state.editor;
  const host = ed?.body.querySelector('#agent-ed-runs-host');
  if (!host || !ed.agent) return;
  host.innerHTML = `<div class="agents-tree-empty">${escapeHtml(I18n.t('agents.runs_loading'))}</div>`;
  let runs = [];
  try {
    const resp = await ApiBinary.one('agentRunsListRequest', { agentId: ed.agent.id });
    runs = JSON.parse(resp.runsJson ?? resp.runs_json ?? '[]');
  } catch (err) {
    host.innerHTML = `<div class="agents-form-error">${escapeHtml(`${I18n.t('agents.runs_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (!Array.isArray(runs) || !runs.length) {
    host.innerHTML = `<tf-empty-state icon="clock" title="${escapeAttr(I18n.t('agents.runs_empty'))}"></tf-empty-state>`;
    return;
  }

  host.innerHTML = `
    <tf-table id="agent-runs-table" sortable>
      <tf-column key="status" label="${escapeAttr(I18n.t('agents.runs_col_status'))}" renderer="chip"></tf-column>
      <tf-column key="iterations" label="${escapeAttr(I18n.t('agents.runs_col_iterations'))}" renderer="num" sortable></tf-column>
      <tf-column key="tokens" label="${escapeAttr(I18n.t('agents.runs_col_tokens'))}" renderer="num" sortable></tf-column>
      <tf-column key="exit_reason" label="${escapeAttr(I18n.t('agents.runs_col_exit'))}"></tf-column>
      <tf-column key="created" label="${escapeAttr(I18n.t('agents.runs_col_created'))}" sortable></tf-column>
    </tf-table>
    <div id="agent-run-detail-host" class="agents-run-detail"></div>
  `;

  const table = host.querySelector('#agent-runs-table');
  table.rows = runs.map((r) => ({
    _id: r.id,
    status: { status: RUN_STATUS_CHIP[r.status] || 'info', label: runStatusLabel(r.status) },
    iterations: r.iterations ?? 0,
    tokens: r.total_tokens ?? 0,
    exit_reason: r.exit_reason || '—',
    created: formatTimestamp(r.created_at),
  }));
  table.addEventListener('row-click', (e) => {
    const id = e.detail?.row?._id;
    if (id) loadRunDetail(id);
  });
}

async function loadRunDetail(runId) {
  const host = state.editor?.body.querySelector('#agent-run-detail-host');
  if (!host) return;
  host.innerHTML = `<div class="agents-tree-empty">${escapeHtml(I18n.t('agents.runs_loading'))}</div>`;
  let run = null;
  try {
    const resp = await ApiBinary.one('agentRunDetailRequest', { runId });
    run = JSON.parse(resp.runJson ?? resp.run_json ?? 'null');
  } catch (err) {
    host.innerHTML = `<div class="agents-form-error">${escapeHtml(`${I18n.t('agents.run_detail_failed')}: ${err.message}`)}</div>`;
    return;
  }
  if (!run) return;
  host.innerHTML = renderRunTimeline(run);
}

function runStatusLabel(status) {
  return RUN_STATUS_CHIP[status] ? I18n.t(`agents.run_status_${status}`) : status;
}

// The run_log is a JSON array of step objects written by the harness blocks
// (agent_context / tool_exec). We render whatever structured fields are present
// without assuming a fixed schema — the timeline degrades gracefully.
function renderRunTimeline(run) {
  let steps = [];
  try {
    const parsed = JSON.parse(run.run_log || '[]');
    if (Array.isArray(parsed)) steps = parsed;
  } catch {
    steps = [];
  }

  const meta = `
    <div class="agents-run-meta">
      <tf-chip status="${RUN_STATUS_CHIP[run.status] || 'info'}">${escapeHtml(runStatusLabel(run.status))}</tf-chip>
      <span>${escapeHtml(I18n.t('agents.runs_col_iterations'))}: ${escapeHtml(String(run.iterations ?? 0))}</span>
      <span>${escapeHtml(I18n.t('agents.runs_col_tokens'))}: ${escapeHtml(String(run.total_tokens ?? 0))}</span>
      ${run.exit_reason ? `<span>${escapeHtml(I18n.t('agents.runs_col_exit'))}: ${escapeHtml(run.exit_reason)}</span>` : ''}
    </div>
  `;

  const prompt = run.prompt
    ? `<div class="agents-run-block"><span class="tf-label">${escapeHtml(I18n.t('agents.run_prompt'))}</span><pre>${escapeHtml(run.prompt)}</pre></div>`
    : '';
  const result = run.result
    ? `<div class="agents-run-block"><span class="tf-label">${escapeHtml(I18n.t('agents.run_result'))}</span><pre>${escapeHtml(run.result)}</pre></div>`
    : '';

  const timeline = steps.length
    ? `<ol class="agents-run-timeline">${steps.map(renderRunStep).join('')}</ol>`
    : `<div class="agents-tree-empty">${escapeHtml(I18n.t('agents.run_no_steps'))}</div>`;

  return `${meta}${prompt}${result}
    <div class="agents-run-block"><span class="tf-label">${escapeHtml(I18n.t('agents.run_timeline'))}</span>${timeline}</div>`;
}

function renderRunStep(step) {
  const kind = step?.kind || step?.type || I18n.t('agents.run_step');
  const detail = step?.detail || step?.message || step?.tool || '';
  const ts = step?.at || step?.timestamp || '';
  return `
    <li class="agents-run-step">
      <span class="agents-run-step-kind">${escapeHtml(String(kind))}</span>
      ${detail ? `<span class="agents-run-step-detail">${escapeHtml(String(detail))}</span>` : ''}
      ${ts ? `<span class="agents-run-step-ts">${escapeHtml(formatTimestamp(String(ts)))}</span>` : ''}
    </li>
  `;
}

// =============================================================================
// Picker data loaders
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

async function loadModels() {
  try {
    const list = await ApiBinary.list('modelListRequest', { arrayKey: 'models' });
    return (Array.isArray(list) ? list : []).map((m) => {
      const value = m.model_name || m.modelName || '';
      const display = m.display_name || m.displayName || value;
      const engine = m.engine_id || m.engineId;
      return { value, label: engine ? `${display} (${engine})` : display };
    }).filter((o) => o.value);
  } catch {
    return [];
  }
}

// =============================================================================
// Save / validation
// =============================================================================

function wireEditor(foot) {
  const ed = state.editor;

  ed.body.querySelector('#agent-ed-tabs')?.addEventListener('change', (e) => {
    const id = e.detail?.value;
    if (id) switchEditorTab(id);
  });

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') ed.cleanup();
    else if (btn.dataset.action === 'save') await saveEditor();
  });

  ed.win.addEventListener('action', (e) => {
    if (e.detail?.action === 'close') ed.cleanup();
  });
}

function showFormError(message) {
  const el = state.editor?.body.querySelector('[data-form-error]');
  if (!el) {
    toast(message, 'error');
    return;
  }
  el.hidden = false;
  el.textContent = message;
}

function intField(sel, fallback) {
  const raw = state.editor?.body.querySelector(sel)?.value;
  const n = Number.parseInt(raw, 10);
  return Number.isFinite(n) ? n : fallback;
}

async function saveEditor() {
  const ed = state.editor;
  if (!ed) return;
  const field = (sel) => ed.body.querySelector(sel);

  const name = (field('#agent-ed-name')?.value || '').trim();
  const displayName = (field('#agent-ed-display-name')?.value || '').trim();
  const description = (field('#agent-ed-description')?.value || '').trim();
  const systemPrompt = (field('#agent-ed-system-prompt')?.value || '').trim();
  const model = (field('#agent-ed-model')?.value || '').trim();
  const flowId = (field('#agent-ed-flow-id')?.value || '').trim();
  const maxIterations = intField('#agent-ed-max-iterations', 25);
  const timeoutSecs = intField('#agent-ed-timeout-secs', 600);
  const maxSubagents = intField('#agent-ed-max-subagents', 0);
  const maxSpawnDepth = intField('#agent-ed-max-spawn-depth', 1);
  const routable = field('#agent-ed-routable')?.hasAttribute('checked') ?? true;
  const isEnabled = field('#agent-ed-enabled')?.hasAttribute('checked') ?? true;

  const error = validateEditableFields(name, description, maxIterations, timeoutSecs, maxSpawnDepth);
  if (error) {
    showFormError(error);
    return;
  }

  const payload = {
    name,
    display_name: displayName || null,
    description,
    system_prompt: systemPrompt || null,
    model: model || null,
    tools: [...ed.selectedTools],
    skills: {
      names: [...ed.selectedSkillNames],
      tags: [...ed.selectedSkillTags],
    },
    params: ed.agent ? safeParseParams(ed.agent.params_json) : {},
    max_iterations: maxIterations,
    timeout_secs: timeoutSecs,
    max_subagents: maxSubagents,
    max_spawn_depth: maxSpawnDepth,
    flow_id: flowId || null,
    routable,
    is_enabled: isEnabled,
  };
  if (ed.mode === 'edit') payload.id = ed.agent.id;

  try {
    await ApiBinary.one('agentsUpsertRequest', { agentJson: JSON.stringify(payload) });
    toast(I18n.t(ed.mode === 'create' ? 'agents.create_ok' : 'agents.save_ok'), 'success');
    ed.cleanup();
    await loadAgents();
  } catch (err) {
    showFormError(err.message);
  }
}

function safeParseParams(json) {
  try {
    const obj = JSON.parse(json || '{}');
    return obj && typeof obj === 'object' && !Array.isArray(obj) ? obj : {};
  } catch {
    return {};
  }
}

function validateEditableFields(name, description, maxIterations, timeoutSecs, maxSpawnDepth) {
  if (!KEBAB_REGEX.test(name) || name.length > NAME_MAX_CHARS) {
    return I18n.t('agents.err_name', { max: NAME_MAX_CHARS });
  }
  if (!description) {
    return I18n.t('agents.err_description');
  }
  if (maxIterations < 1 || maxIterations > MAX_ITERATIONS_CAP) {
    return I18n.t('agents.err_max_iterations', { max: MAX_ITERATIONS_CAP });
  }
  if (timeoutSecs < 1) {
    return I18n.t('agents.err_timeout');
  }
  if (maxSpawnDepth < 1) {
    return I18n.t('agents.err_max_spawn_depth');
  }
  return null;
}
