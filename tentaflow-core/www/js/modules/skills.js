// ===== File: skills.js — Skills registry admin screen (Harness plan §3.2) =====
//
// List + CRUD UI for the `skills` table over the binary protocol
// (`MessageBody::SkillsBody` via ApiBinary + stage-B codec helpers).
// Addon-sourced skills are package-owned: name/description/content/category/
// display_name are read-only in the editor (only tags + status are editable,
// mirroring the handler-side immutability check) and the editor offers
// "Fork as my skill" (SkillsForkRequest) for an editable user copy.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-table.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-checkbox.js';

// Limits mirror db::repository SKILL_* constants so violations surface inline
// instead of as backend bad_request round-trips.
const NAME_MAX_CHARS = 64;
const DESCRIPTION_MAX_CHARS = 1024;
const CONTENT_MAX_CHARS = 100000;
const KEBAB_REGEX = /^[a-z0-9]+(-[a-z0-9]+)*$/;

const SOURCE_VALUES = ['user', 'addon', 'hub'];
const STATUS_VALUES = ['active', 'disabled', 'quarantine', 'archived'];

const SOURCE_CHIP = { user: 'info', addon: 'accent', hub: 'warn' };
const STATUS_CHIP = { active: 'ok', disabled: 'warn', quarantine: 'err', archived: 'info' };

const state = {
  skills: [],
  searchQuery: '',
  sourceFilter: 'all',
  tagFilter: 'all',
  editor: null,
  forkWin: null,
  topTab: 'registry',
  hubResults: [],
  hubBusy: false,
  curatorBusy: false,
  curatorProposal: null,
  curatorSnapshotId: null,
  curatorApproved: new Set(),
  curatorAppliedSnapshotId: null,
};

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

const SkillsScreen = {
  get title() { return I18n.t('skills.title'); },

  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('sparkle')} ${escapeHtml(I18n.t('skills.title'))}</h1>
          <div class="sub" id="skills-sub"></div>
        </div>
        <div class="actions">
          <tf-button variant="ghost" icon="refresh" id="skills-refresh">${escapeHtml(I18n.t('skills.refresh'))}</tf-button>
          <tf-button variant="primary" icon="plus" id="skills-new">${escapeHtml(I18n.t('skills.new_skill'))}</tf-button>
        </div>
      </div>

      <tf-tabs variant="underline" value="registry" id="skills-top-tabs">
        <tf-tab id="registry" icon="sparkle">${escapeHtml(I18n.t('skills.tab_registry'))}</tf-tab>
        <tf-tab id="hub" icon="download">${escapeHtml(I18n.t('skills.tab_hub'))}</tf-tab>
        <tf-tab id="curator" icon="cluster">${escapeHtml(I18n.t('skills.tab_curator'))}</tf-tab>
      </tf-tabs>

      <div class="skills-top-panel" data-top-panel="registry">
        <section class="card skills-card">
          <div class="skills-toolbar">
            <tf-searchbox id="skills-search" placeholder="${escapeAttr(I18n.t('skills.search_placeholder'))}" debounce="200"></tf-searchbox>
            <tf-select id="skills-filter-source" class="skills-filter" value="all">
              <option value="all">${escapeHtml(I18n.t('skills.filter_source_all'))}</option>
              ${SOURCE_VALUES.map((s) => `<option value="${escapeAttr(s)}">${escapeHtml(sourceLabel(s))}</option>`).join('')}
            </tf-select>
            <span id="skills-filter-tag-slot"></span>
          </div>
          <div id="skills-table-host" class="skills-table-host"></div>
        </section>
      </div>

      <div class="skills-top-panel" data-top-panel="hub" hidden>
        <section class="card skills-card">
          <div class="skills-toolbar">
            <tf-searchbox id="hub-search" placeholder="${escapeAttr(I18n.t('skills.hub.search_placeholder'))}" debounce="0"></tf-searchbox>
            <tf-input id="hub-source" placeholder="${escapeAttr(I18n.t('skills.hub.source_placeholder'))}"></tf-input>
            <span class="skills-toolbar-spacer"></span>
            <tf-button variant="ghost" icon="download" id="hub-import-direct">${escapeHtml(I18n.t('skills.hub.import_direct'))}</tf-button>
            <tf-button variant="primary" icon="search" id="hub-search-btn">${escapeHtml(I18n.t('skills.hub.search_action'))}</tf-button>
          </div>
          <div class="skills-hub-hint">${escapeHtml(I18n.t('skills.hub.hint'))}</div>
          <div id="hub-results-host" class="skills-table-host"></div>
        </section>
      </div>

      <div class="skills-top-panel" data-top-panel="curator" hidden>
        <section class="card skills-card">
          <div class="skills-toolbar">
            <span class="skills-toolbar-spacer"></span>
            <tf-button variant="ghost" icon="rotate" id="curator-rollback" disabled>${escapeHtml(I18n.t('skills.curator.rollback_action'))}</tf-button>
            <tf-button variant="primary" icon="cluster" id="curator-run">${escapeHtml(I18n.t('skills.curator.run_action'))}</tf-button>
          </div>
          <div class="skills-hub-hint">${escapeHtml(I18n.t('skills.curator.hint'))}</div>
          <div id="curator-host" class="skills-table-host"></div>
        </section>
      </div>
    `;
  },

  async mount() {
    byId('skills-refresh')?.addEventListener('click', () => {
      if (state.topTab === 'hub') runHubSearch();
      else loadSkills();
    });
    byId('skills-new')?.addEventListener('click', () => openSkillEditor(null));
    byId('skills-search')?.addEventListener('search', (e) => {
      state.searchQuery = String(e.detail?.value ?? '');
      renderTable();
    });
    byId('skills-filter-source')?.addEventListener('change', (e) => {
      state.sourceFilter = e.detail?.value ?? e.target.value ?? 'all';
      renderTable();
    });
    byId('skills-top-tabs')?.addEventListener('change', (e) => {
      const id = e.detail?.value;
      if (id) switchTopTab(id);
    });
    byId('hub-search-btn')?.addEventListener('click', () => runHubSearch());
    byId('hub-search')?.addEventListener('search', () => runHubSearch());
    byId('hub-import-direct')?.addEventListener('click', () => importDirectFromSource());
    byId('curator-run')?.addEventListener('click', () => runCuratorReview());
    byId('curator-rollback')?.addEventListener('click', () => rollbackCurator());
    renderTagFilter();
    renderCurator();
    await loadSkills();
  },

  unmount() {
    state.editor?.cleanup();
    if (state.forkWin?.isConnected) state.forkWin.close(true);
    state.forkWin = null;
    state.skills = [];
    state.searchQuery = '';
    state.sourceFilter = 'all';
    state.tagFilter = 'all';
    state.topTab = 'registry';
    state.hubResults = [];
    state.hubBusy = false;
    state.curatorBusy = false;
    state.curatorProposal = null;
    state.curatorSnapshotId = null;
    state.curatorApproved = new Set();
    state.curatorAppliedSnapshotId = null;
  },
};

const TOP_TABS = ['registry', 'hub', 'curator'];

function switchTopTab(tabId) {
  state.topTab = TOP_TABS.includes(tabId) ? tabId : 'registry';
  document.querySelectorAll('.skills-top-panel').forEach((panel) => {
    panel.hidden = panel.getAttribute('data-top-panel') !== state.topTab;
  });
}

export default SkillsScreen;

// =============================================================================
// Data
// =============================================================================

async function loadSkills() {
  try {
    const resp = await ApiBinary.one('skillsListRequest', {});
    const rows = JSON.parse(resp.skillsJson ?? resp.skills_json ?? '[]');
    state.skills = Array.isArray(rows) ? rows : [];
    const sub = byId('skills-sub');
    if (sub) sub.textContent = I18n.t('skills.subtitle', { count: state.skills.length });
    renderTagFilter();
    renderTable();
  } catch (err) {
    toast(`${I18n.t('skills.load_failed')}: ${err.message}`, 'error');
  }
}

function parseTags(tagsJson) {
  try {
    const arr = JSON.parse(tagsJson || '[]');
    return Array.isArray(arr) ? arr.filter((t) => typeof t === 'string') : [];
  } catch {
    return [];
  }
}

function allTags() {
  const set = new Set();
  for (const skill of state.skills) {
    for (const tag of parseTags(skill.tags_json)) set.add(tag);
  }
  return [...set].sort();
}

function filteredSkills() {
  const query = state.searchQuery.trim().toLowerCase();
  return state.skills.filter((skill) => {
    if (state.sourceFilter !== 'all' && skill.source !== state.sourceFilter) return false;
    const tags = parseTags(skill.tags_json);
    if (state.tagFilter !== 'all' && !tags.includes(state.tagFilter)) return false;
    if (query) {
      const haystack = [skill.name, skill.display_name || '', skill.description || '', tags.join(' ')]
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

// Rebuilds the tag <tf-select> from scratch on every data refresh: the
// component captures its <option> children once at connect, so options must
// exist before insertion (mutating innerHTML afterwards would wipe its DOM).
function renderTagFilter() {
  const slot = byId('skills-filter-tag-slot');
  if (!slot) return;
  const tags = allTags();
  if (state.tagFilter !== 'all' && !tags.includes(state.tagFilter)) state.tagFilter = 'all';
  const select = document.createElement('tf-select');
  select.className = 'skills-filter';
  select.setAttribute('value', state.tagFilter);
  select.innerHTML = [
    `<option value="all">${escapeHtml(I18n.t('skills.filter_tag_all'))}</option>`,
    ...tags.map((t) => `<option value="${escapeAttr(t)}">${escapeHtml(t)}</option>`),
  ].join('');
  select.addEventListener('change', (e) => {
    state.tagFilter = e.detail?.value ?? 'all';
    renderTable();
  });
  slot.replaceChildren(select);
}

function sourceLabel(source) {
  return SOURCE_VALUES.includes(source) ? I18n.t(`skills.source_${source}`) : source;
}

function statusLabel(status) {
  return STATUS_VALUES.includes(status) ? I18n.t(`skills.status_${status}`) : status;
}

function renderTable() {
  const host = byId('skills-table-host');
  if (!host) return;
  const visible = filteredSkills();

  if (!visible.length) {
    const noSkillsAtAll = state.skills.length === 0;
    host.innerHTML = `
      <tf-empty-state icon="sparkle"
        title="${escapeAttr(I18n.t(noSkillsAtAll ? 'skills.empty_list' : 'skills.empty_match'))}"
        message="${escapeAttr(noSkillsAtAll ? I18n.t('skills.empty_list_hint') : '')}"></tf-empty-state>
    `;
    return;
  }

  host.innerHTML = `
    <tf-table id="skills-table" sortable>
      <tf-column key="name" label="${escapeAttr(I18n.t('skills.col_name'))}" sortable></tf-column>
      <tf-column key="display_name" label="${escapeAttr(I18n.t('skills.col_display_name'))}" sortable></tf-column>
      <tf-column key="tags" label="${escapeAttr(I18n.t('skills.col_tags'))}" renderer="html"></tf-column>
      <tf-column key="source" label="${escapeAttr(I18n.t('skills.col_source'))}" renderer="chip"></tf-column>
      <tf-column key="status" label="${escapeAttr(I18n.t('skills.col_status'))}" renderer="chip"></tf-column>
      <tf-column key="use_count" label="${escapeAttr(I18n.t('skills.col_use_count'))}" renderer="num" sortable></tf-column>
      <tf-column key="updated" label="${escapeAttr(I18n.t('skills.col_updated'))}"></tf-column>
    </tf-table>
  `;

  const table = byId('skills-table');
  table.rows = visible.map(skillToRow);
  // Row-action buttons are created as live elements (not via the html cell
  // renderer): cells live inside tf-table's shadow root, where event
  // retargeting would hide the clicked button from a host-level listener.
  table.rowActions = buildRowActions;
  table.addEventListener('row-click', (e) => {
    const id = e.detail?.row?._id;
    if (id) openSkillEditor(id);
  });
}

function skillToRow(skill) {
  const tags = parseTags(skill.tags_json);
  return {
    _id: skill.id,
    name: skill.name,
    display_name: skill.display_name || '',
    tags: tags
      .map((t) => `<span class="tf-chip info" style="margin: 1px 2px;">${escapeHtml(t)}</span>`)
      .join(''),
    source: { status: SOURCE_CHIP[skill.source] || 'info', label: sourceLabel(skill.source) },
    status: { status: STATUS_CHIP[skill.status] || 'info', label: statusLabel(skill.status) },
    use_count: skill.use_count ?? 0,
    updated: formatTimestamp(skill.updated_at),
  };
}

function buildRowActions(row) {
  const wrap = document.createElement('div');
  wrap.style.display = 'flex';
  wrap.style.gap = '4px';
  wrap.style.justifyContent = 'flex-end';

  // Text labels instead of icons: tf-table's shadow root has no sprite copy,
  // so <use href="#i-..."> inside it would not resolve.
  const edit = document.createElement('tf-button');
  edit.setAttribute('variant', 'ghost');
  edit.setAttribute('size', 'sm');
  edit.textContent = I18n.t('skills.action_edit');
  edit.addEventListener('click', () => openSkillEditor(row._id));
  wrap.appendChild(edit);

  const del = document.createElement('tf-button');
  del.setAttribute('variant', 'danger');
  del.setAttribute('size', 'sm');
  del.textContent = I18n.t('skills.action_delete');
  del.addEventListener('click', () => deleteSkill(row._id));
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

async function deleteSkill(skillId) {
  const skill = state.skills.find((s) => s.id === skillId);
  if (!skill) return;
  const ok = await TfWindow.confirm({
    title: I18n.t('skills.delete_confirm_title'),
    message: I18n.t('skills.delete_confirm_message', { name: escapeHtml(skill.name) }),
    confirmLabel: I18n.t('skills.action_delete'),
    cancelLabel: I18n.t('skills.action_cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await ApiBinary.one('skillsDeleteRequest', { skillId });
    toast(I18n.t('skills.delete_ok'), 'success');
    await loadSkills();
  } catch (err) {
    // Addon-sourced skills are rejected server-side ("uninstall the addon...")
    // — the backend message is the user-facing explanation.
    toast(`${I18n.t('skills.delete_failed')}: ${err.message}`, 'error');
  }
}

// =============================================================================
// Editor (tf-window)
// =============================================================================

async function openSkillEditor(skillId) {
  state.editor?.cleanup();

  let skill = null;
  let files = [];
  if (skillId) {
    try {
      const resp = await ApiBinary.one('skillsDetailRequest', { skillId });
      skill = JSON.parse(resp.skillJson ?? resp.skill_json ?? 'null');
      files = JSON.parse(resp.filesJson ?? resp.files_json ?? '[]');
    } catch (err) {
      toast(`${I18n.t('skills.detail_failed')}: ${err.message}`, 'error');
      return;
    }
    if (!skill) return;
  }

  const mode = skill ? 'edit' : 'create';
  const isAddon = skill?.source === 'addon';

  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t(mode === 'create' ? 'skills.editor_create_title' : 'skills.editor_edit_title'));
  if (skill) win.setAttribute('subtitle', skill.name);
  win.setAttribute('icon', 'sparkle');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '860');
  win.setAttribute('draggable', '');

  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'skills-editor';
  body.innerHTML = editorBodyHtml(skill, files, isAddon);
  win.appendChild(body);

  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.className = 'skills-editor-footer';
  foot.innerHTML = editorFooterHtml(mode, isAddon);
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
    skill,
    tags: skill ? parseTags(skill.tags_json) : [],
    win,
    body,
    cleanup,
  };

  // Long markdown is injected via the value property after the template parse
  // so the 100k-char string never goes through escapeAttr + innerHTML parsing
  // (tf-textarea still mirrors value to its attribute — component-wide trait).
  const description = body.querySelector('#skill-ed-description');
  if (description) description.value = skill?.description || '';
  const content = body.querySelector('#skill-ed-content');
  if (content) content.value = skill?.content || '';

  renderTagChips();
  wireEditor(foot);
}

function editorBodyHtml(skill, files, isAddon) {
  const lockAttr = isAddon ? 'disabled' : '';
  const addonHint = isAddon
    ? `<div class="skills-addon-hint">${sprite('info')}<span>${escapeHtml(I18n.t('skills.hint_addon_readonly'))}</span></div>`
    : '';
  const filesBlock = files.length
    ? `
      <div class="skills-field">
        <span class="tf-label">${escapeHtml(I18n.t('skills.files_title'))}</span>
        <div class="skills-files">
          ${files.map((f) => `<span class="tf-chip info">${escapeHtml(f.path)}</span>`).join('')}
        </div>
        <div class="skills-field-hint">${escapeHtml(I18n.t('skills.files_hint'))}</div>
      </div>
    `
    : '';
  const statusOptions = STATUS_VALUES
    .map((s) => `<option value="${escapeAttr(s)}">${escapeHtml(statusLabel(s))}</option>`)
    .join('');

  return `
    ${addonHint}
    <div class="skills-editor-grid">
      <tf-input id="skill-ed-name" label="${escapeAttr(I18n.t('skills.label_name'))}"
        value="${escapeAttr(skill?.name || '')}"
        hint="${escapeAttr(I18n.t('skills.hint_name'))}"
        maxlength="${NAME_MAX_CHARS}" ${lockAttr}></tf-input>
      <tf-input id="skill-ed-display-name" label="${escapeAttr(I18n.t('skills.label_display_name'))}"
        value="${escapeAttr(skill?.display_name || '')}" ${lockAttr}></tf-input>
      <tf-input id="skill-ed-category" label="${escapeAttr(I18n.t('skills.label_category'))}"
        value="${escapeAttr(skill?.category || '')}" ${lockAttr}></tf-input>
      <tf-select id="skill-ed-status" label="${escapeAttr(I18n.t('skills.label_status'))}"
        value="${escapeAttr(skill?.status || 'active')}">${statusOptions}</tf-select>
    </div>
    <tf-textarea id="skill-ed-description" label="${escapeAttr(I18n.t('skills.label_description'))}"
      rows="3" maxlength="${DESCRIPTION_MAX_CHARS}" ${lockAttr}></tf-textarea>
    <div class="skills-field">
      <span class="tf-label">${escapeHtml(I18n.t('skills.label_tags'))}</span>
      <div id="skill-ed-tags" class="skills-tags-editor"></div>
      <div class="skills-tag-add">
        <tf-input id="skill-ed-tag-input" placeholder="${escapeAttr(I18n.t('skills.tag_placeholder'))}"></tf-input>
        <tf-button variant="secondary" size="sm" id="skill-ed-tag-add">${escapeHtml(I18n.t('skills.tag_add'))}</tf-button>
      </div>
    </div>
    <div class="skills-field">
      <span class="tf-label">${escapeHtml(I18n.t('skills.label_content'))}</span>
      <tf-code-editor id="skill-ed-content" class="skills-content"
        language="markdown" ${isAddon ? 'readonly' : ''}></tf-code-editor>
    </div>
    ${filesBlock}
    <div class="skills-form-error" data-form-error hidden></div>
  `;
}

function editorFooterHtml(mode, isAddon) {
  const fork = mode === 'edit' && isAddon
    ? `<tf-button variant="secondary" data-action="fork">${escapeHtml(I18n.t('skills.action_fork'))}</tf-button>`
    : '';
  const saveLabel = I18n.t(mode === 'create' ? 'skills.action_create' : 'skills.action_save');
  return `
    <div class="skills-footer-left">${fork}</div>
    <div class="skills-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('skills.action_cancel'))}</tf-button>
      <tf-button variant="primary" data-action="save">${escapeHtml(saveLabel)}</tf-button>
    </div>
  `;
}

function renderTagChips() {
  const host = state.editor?.body.querySelector('#skill-ed-tags');
  if (!host) return;
  const tags = state.editor.tags;
  host.innerHTML = tags.length
    ? tags
      .map((t) => `<tf-chip status="info" clickable data-remove-tag="${escapeAttr(t)}" title="${escapeAttr(I18n.t('skills.tag_remove'))}">${escapeHtml(t)} ×</tf-chip>`)
      .join('')
    : `<span class="skills-tags-empty">${escapeHtml(I18n.t('skills.tags_empty'))}</span>`;
}

function addTagFromInput() {
  const ed = state.editor;
  if (!ed) return;
  const input = ed.body.querySelector('#skill-ed-tag-input');
  const tag = (input?.value || '').trim();
  if (!tag) return;
  if (!ed.tags.includes(tag)) {
    ed.tags.push(tag);
    renderTagChips();
  }
  input.value = '';
  input.focus();
}

function wireEditor(foot) {
  const ed = state.editor;

  ed.body.addEventListener('click', (e) => {
    const chip = e.target.closest('[data-remove-tag]');
    if (chip) {
      ed.tags = ed.tags.filter((t) => t !== chip.dataset.removeTag);
      renderTagChips();
      return;
    }
    if (e.target.closest('#skill-ed-tag-add')) addTagFromInput();
  });
  ed.body.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter') return;
    if (e.target.closest('#skill-ed-tag-input')) {
      e.preventDefault();
      addTagFromInput();
    }
  });

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    const action = btn.dataset.action;
    if (action === 'cancel') ed.cleanup();
    else if (action === 'fork') openForkDialog(ed.skill);
    else if (action === 'save') await saveEditor();
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

function validateEditableFields(name, description, content) {
  if (!KEBAB_REGEX.test(name) || name.length > NAME_MAX_CHARS) {
    return I18n.t('skills.err_name', { max: NAME_MAX_CHARS });
  }
  if (!description || description.length > DESCRIPTION_MAX_CHARS) {
    return I18n.t('skills.err_description', { max: DESCRIPTION_MAX_CHARS });
  }
  if (!content || content.length > CONTENT_MAX_CHARS) {
    return I18n.t('skills.err_content', { max: CONTENT_MAX_CHARS });
  }
  return null;
}

async function saveEditor() {
  const ed = state.editor;
  if (!ed) return;
  const isAddon = ed.skill?.source === 'addon';
  const field = (sel) => ed.body.querySelector(sel);

  // Package-owned fields of addon skills are echoed back verbatim — the
  // backend rejects any change to them, only tags/status may differ.
  const name = isAddon ? ed.skill.name : (field('#skill-ed-name')?.value || '').trim();
  const displayName = isAddon ? (ed.skill.display_name || '') : (field('#skill-ed-display-name')?.value || '').trim();
  const category = isAddon ? (ed.skill.category || '') : (field('#skill-ed-category')?.value || '').trim();
  const description = isAddon ? ed.skill.description : (field('#skill-ed-description')?.value || '').trim();
  const content = isAddon ? ed.skill.content : (field('#skill-ed-content')?.value || '');
  const status = field('#skill-ed-status')?.value || 'active';

  if (!isAddon) {
    const error = validateEditableFields(name, description, content);
    if (error) {
      showFormError(error);
      return;
    }
  }

  const payload = {
    name,
    display_name: displayName || null,
    description,
    content,
    tags: ed.tags,
    category: category || null,
    status,
  };
  if (ed.mode === 'edit') payload.id = ed.skill.id;

  try {
    await ApiBinary.one('skillsUpsertRequest', { skillJson: JSON.stringify(payload) });
    toast(I18n.t(ed.mode === 'create' ? 'skills.create_ok' : 'skills.save_ok'), 'success');
    ed.cleanup();
    await loadSkills();
  } catch (err) {
    showFormError(err.message);
  }
}

// =============================================================================
// Fork (addon skill → independent user copy)
// =============================================================================

function defaultForkName(name) {
  const base = name.slice(0, NAME_MAX_CHARS - 5).replace(/-+$/, '');
  return `${base}-copy`;
}

function openForkDialog(skill) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('skills.fork_title'));
  win.setAttribute('subtitle', skill.name);
  win.setAttribute('icon', 'sparkle');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '460');
  win.setAttribute('draggable', '');

  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'skills-editor';
  body.innerHTML = `
    <tf-input id="skill-fork-name" label="${escapeAttr(I18n.t('skills.fork_name_label'))}"
      value="${escapeAttr(defaultForkName(skill.name))}"
      hint="${escapeAttr(I18n.t('skills.hint_name'))}"
      maxlength="${NAME_MAX_CHARS}"></tf-input>
    <div class="skills-form-error" data-form-error hidden></div>
  `;
  win.appendChild(body);

  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.className = 'skills-editor-footer';
  foot.innerHTML = `
    <div class="skills-footer-left"></div>
    <div class="skills-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('skills.action_cancel'))}</tf-button>
      <tf-button variant="primary" data-action="fork">${escapeHtml(I18n.t('skills.fork_submit'))}</tf-button>
    </div>
  `;
  win.appendChild(foot);

  document.body.appendChild(win);
  state.forkWin = win;
  win.addEventListener('close-request', () => {
    if (state.forkWin === win) state.forkWin = null;
  });

  const forkError = (message) => {
    const el = body.querySelector('[data-form-error]');
    el.hidden = false;
    el.textContent = message;
  };

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') {
      win.close(true);
      if (state.forkWin === win) state.forkWin = null;
      return;
    }
    const newName = (body.querySelector('#skill-fork-name')?.value || '').trim();
    if (!KEBAB_REGEX.test(newName) || newName.length > NAME_MAX_CHARS) {
      forkError(I18n.t('skills.err_name', { max: NAME_MAX_CHARS }));
      return;
    }
    try {
      const resp = await ApiBinary.one('skillsForkRequest', { skillId: skill.id, newName });
      toast(I18n.t('skills.fork_ok'), 'success');
      win.close(true);
      if (state.forkWin === win) state.forkWin = null;
      state.editor?.cleanup();
      await loadSkills();
      const newId = resp.skillId ?? resp.skill_id;
      if (newId) openSkillEditor(newId);
    } catch (err) {
      forkError(`${I18n.t('skills.fork_failed')}: ${err.message}`);
    }
  });
}

// =============================================================================
// Hub (Harness plan §3.2 source `hub`) — search a tap, import → quarantine +
// scan verdict, admin approve/reject.
// =============================================================================

async function runHubSearch() {
  if (state.hubBusy) return;
  const query = (byId('hub-search')?.value || '').trim();
  const source = (byId('hub-source')?.value || '').trim();
  state.hubBusy = true;
  renderHubResults(true);
  try {
    const resp = await ApiBinary.one('skillsHubSearchRequest', {
      query,
      source: source || undefined,
    });
    const rows = JSON.parse(resp.resultsJson ?? resp.results_json ?? '[]');
    state.hubResults = Array.isArray(rows) ? rows : [];
    renderHubResults(false);
  } catch (err) {
    toast(`${I18n.t('skills.hub.search_failed')}: ${err.message}`, 'error');
    renderHubResults(false);
  } finally {
    state.hubBusy = false;
  }
}

function renderHubResults(busy) {
  const host = byId('hub-results-host');
  if (!host) return;
  if (busy) {
    host.innerHTML = `<tf-empty-state icon="download" title="${escapeAttr(I18n.t('skills.hub.searching'))}"></tf-empty-state>`;
    return;
  }
  if (!state.hubResults.length) {
    host.innerHTML = `<tf-empty-state icon="download" title="${escapeAttr(I18n.t('skills.hub.empty'))}" message="${escapeAttr(I18n.t('skills.hub.empty_hint'))}"></tf-empty-state>`;
    return;
  }
  host.innerHTML = `
    <tf-table id="hub-table">
      <tf-column key="name" label="${escapeAttr(I18n.t('skills.col_name'))}"></tf-column>
      <tf-column key="description" label="${escapeAttr(I18n.t('skills.label_description'))}"></tf-column>
      <tf-column key="tags" label="${escapeAttr(I18n.t('skills.col_tags'))}" renderer="html"></tf-column>
      <tf-column key="source" label="${escapeAttr(I18n.t('skills.col_source'))}"></tf-column>
    </tf-table>
  `;
  const table = byId('hub-table');
  table.rows = state.hubResults.map((r, i) => ({
    _idx: i,
    name: r.name || '',
    description: r.description || '',
    tags: (Array.isArray(r.tags) ? r.tags : [])
      .map((t) => `<span class="tf-chip info" style="margin: 1px 2px;">${escapeHtml(t)}</span>`)
      .join(''),
    source: r.source || '',
  }));
  table.rowActions = (row) => {
    const wrap = document.createElement('div');
    wrap.style.display = 'flex';
    wrap.style.justifyContent = 'flex-end';
    const imp = document.createElement('tf-button');
    imp.setAttribute('variant', 'primary');
    imp.setAttribute('size', 'sm');
    imp.textContent = I18n.t('skills.hub.import_action');
    imp.addEventListener('click', () => {
      const result = state.hubResults[row._idx];
      if (result) importFromSource(result.source);
    });
    wrap.appendChild(imp);
    return wrap;
  };
}

async function importDirectFromSource() {
  const source = (byId('hub-source')?.value || '').trim();
  if (!source) {
    toast(I18n.t('skills.hub.source_required'), 'error');
    return;
  }
  importFromSource(source);
}

async function importFromSource(source) {
  if (state.hubBusy) return;
  state.hubBusy = true;
  try {
    const resp = await ApiBinary.one('skillsHubImportRequest', { source });
    const skillId = resp.skillId ?? resp.skill_id;
    const verdict = JSON.parse(resp.verdictJson ?? resp.verdict_json ?? '{"clean":true,"findings":[]}');
    toast(I18n.t('skills.hub.import_ok'), 'success');
    await loadSkills();
    openVerdictModal(skillId, source, verdict);
  } catch (err) {
    toast(`${I18n.t('skills.hub.import_failed')}: ${err.message}`, 'error');
  } finally {
    state.hubBusy = false;
  }
}

function findingSeverityChip(severity) {
  if (severity === 'critical') return 'err';
  if (severity === 'high') return 'warn';
  return 'info';
}

function openVerdictModal(skillId, source, verdict) {
  const findings = Array.isArray(verdict.findings) ? verdict.findings : [];
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('skills.hub.verdict_title'));
  win.setAttribute('icon', 'download');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '640');
  win.setAttribute('draggable', '');

  const body = document.createElement('div');
  body.slot = 'body';
  const verdictLine = verdict.clean
    ? `<div class="tf-chip ok">${escapeHtml(I18n.t('skills.hub.verdict_clean'))}</div>`
    : `<div class="tf-chip ${findings.some((f) => f.severity === 'critical') ? 'err' : 'warn'}">${escapeHtml(I18n.t('skills.hub.verdict_flagged', { count: findings.length }))}</div>`;
  const findingsHtml = findings.length
    ? `<ul class="skills-hub-findings">${findings
        .map(
          (f) => `<li>
            <span class="tf-chip ${findingSeverityChip(f.severity)}">${escapeHtml(f.severity || '')}</span>
            <strong>${escapeHtml(f.pattern_id || '')}</strong>
            <span class="muted">${escapeHtml(f.file || '')}:${escapeHtml(String(f.line ?? ''))}</span>
            <div>${escapeHtml(f.description || '')}</div>
            <code>${escapeHtml(f.snippet || '')}</code>
          </li>`,
        )
        .join('')}</ul>`
    : `<p class="muted">${escapeHtml(I18n.t('skills.hub.no_findings'))}</p>`;
  body.innerHTML = `
    <p class="muted">${escapeHtml(I18n.t('skills.hub.verdict_source', { source }))}</p>
    ${verdictLine}
    ${findingsHtml}
  `;
  win.appendChild(body);

  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.innerHTML = `
    <tf-button variant="danger" data-action="reject">${escapeHtml(I18n.t('skills.hub.reject_action'))}</tf-button>
    <tf-button variant="primary" data-action="approve">${escapeHtml(I18n.t('skills.hub.approve_action'))}</tf-button>
  `;
  win.appendChild(foot);

  document.body.appendChild(win);

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    try {
      if (btn.dataset.action === 'approve') {
        await ApiBinary.one('skillsHubApproveRequest', { skillId });
        toast(I18n.t('skills.hub.approve_ok'), 'success');
      } else {
        await ApiBinary.one('skillsHubRejectRequest', { skillId });
        toast(I18n.t('skills.hub.reject_ok'), 'success');
      }
      win.close(true);
      await loadSkills();
    } catch (err) {
      toast(`${I18n.t('skills.hub.action_failed')}: ${err.message}`, 'error');
    }
  });
}

// =============================================================================
// Curator (Harness plan §3.2) — run a review pass (auxiliary LLM proposes
// merge/umbrella/archive actions, no mutation), approve a subset, apply against
// the snapshot, optionally roll back the last apply.
// =============================================================================

async function runCuratorReview() {
  if (state.curatorBusy) return;
  state.curatorBusy = true;
  renderCurator();
  try {
    const resp = await ApiBinary.one('skillsCuratorRunRequest', {});
    const proposal = JSON.parse(resp.proposalJson ?? resp.proposal_json ?? '{"actions":[]}');
    state.curatorProposal = Array.isArray(proposal.actions) ? proposal : { actions: [] };
    state.curatorSnapshotId = resp.snapshotId ?? resp.snapshot_id ?? null;
    state.curatorApproved = new Set(state.curatorProposal.actions.map((_, i) => i));
    state.curatorAppliedSnapshotId = null;
  } catch (err) {
    toast(`${I18n.t('skills.curator.run_failed')}: ${err.message}`, 'error');
  } finally {
    state.curatorBusy = false;
    renderCurator();
  }
}

async function applyCurator() {
  if (state.curatorBusy || !state.curatorSnapshotId) return;
  const approved = [...state.curatorApproved].sort((a, b) => a - b);
  if (!approved.length) {
    toast(I18n.t('skills.curator.none_selected'), 'error');
    return;
  }
  state.curatorBusy = true;
  renderCurator();
  try {
    const resp = await ApiBinary.one('skillsCuratorApplyRequest', {
      snapshotId: state.curatorSnapshotId,
      approvedActions: approved,
    });
    const mutated = Number(resp.mutated ?? 0);
    toast(I18n.t('skills.curator.apply_ok', { count: mutated }), 'success');
    state.curatorAppliedSnapshotId = state.curatorSnapshotId;
    state.curatorProposal = null;
    state.curatorSnapshotId = null;
    state.curatorApproved = new Set();
    await loadSkills();
  } catch (err) {
    toast(`${I18n.t('skills.curator.apply_failed')}: ${err.message}`, 'error');
  } finally {
    state.curatorBusy = false;
    renderCurator();
  }
}

async function rollbackCurator() {
  if (state.curatorBusy || !state.curatorAppliedSnapshotId) return;
  state.curatorBusy = true;
  renderCurator();
  try {
    const resp = await ApiBinary.one('skillsCuratorRollbackRequest', {
      snapshotId: state.curatorAppliedSnapshotId,
    });
    const restored = Number(resp.restored ?? 0);
    toast(I18n.t('skills.curator.rollback_ok', { count: restored }), 'success');
    state.curatorAppliedSnapshotId = null;
    await loadSkills();
  } catch (err) {
    toast(`${I18n.t('skills.curator.rollback_failed')}: ${err.message}`, 'error');
  } finally {
    state.curatorBusy = false;
    renderCurator();
  }
}

function curatorActionLabel(kind) {
  const key = `skills.curator.action_${kind}`;
  const label = I18n.t(key);
  return label === key ? kind : label;
}

function renderCurator() {
  const runBtn = byId('curator-run');
  if (runBtn) runBtn.toggleAttribute('disabled', state.curatorBusy);
  const rollbackBtn = byId('curator-rollback');
  if (rollbackBtn) {
    rollbackBtn.toggleAttribute('disabled', state.curatorBusy || !state.curatorAppliedSnapshotId);
  }

  const host = byId('curator-host');
  if (!host) return;

  if (state.curatorBusy && !state.curatorProposal) {
    host.innerHTML = `<tf-empty-state icon="cluster" title="${escapeAttr(I18n.t('skills.curator.running'))}"></tf-empty-state>`;
    return;
  }
  if (!state.curatorProposal) {
    host.innerHTML = `<tf-empty-state icon="cluster" title="${escapeAttr(I18n.t('skills.curator.empty'))}" message="${escapeAttr(I18n.t('skills.curator.empty_hint'))}"></tf-empty-state>`;
    return;
  }
  const actions = state.curatorProposal.actions;
  if (!actions.length) {
    host.innerHTML = `<tf-empty-state icon="check" title="${escapeAttr(I18n.t('skills.curator.nothing'))}" message="${escapeAttr(I18n.t('skills.curator.nothing_hint'))}"></tf-empty-state>`;
    return;
  }

  const skillNameById = new Map(state.skills.map((s) => [s.id, s.name]));
  const rows = actions
    .map((action, idx) => {
      const members = (Array.isArray(action.skill_ids) ? action.skill_ids : [])
        .map((id) => escapeHtml(skillNameById.get(id) || id))
        .map((n) => `<span class="tf-chip info" style="margin: 1px 2px;">${n}</span>`)
        .join('');
      const target = action.target_name
        ? `<span class="tf-chip ok" style="margin: 1px 2px;">${escapeHtml(action.target_name)}</span>`
        : '<span class="muted">—</span>';
      const checked = state.curatorApproved.has(idx) ? 'checked' : '';
      return `
        <tr>
          <td><tf-checkbox data-action-idx="${idx}" ${checked} ${state.curatorBusy ? 'disabled' : ''}></tf-checkbox></td>
          <td><span class="tf-chip accent">${escapeHtml(curatorActionLabel(action.action))}</span></td>
          <td>${members || '<span class="muted">—</span>'}</td>
          <td>${target}</td>
          <td>${escapeHtml(action.rationale || '')}</td>
        </tr>`;
    })
    .join('');

  host.innerHTML = `
    <table class="skills-curator-table">
      <thead>
        <tr>
          <th>${escapeHtml(I18n.t('skills.curator.col_approve'))}</th>
          <th>${escapeHtml(I18n.t('skills.curator.col_action'))}</th>
          <th>${escapeHtml(I18n.t('skills.curator.col_members'))}</th>
          <th>${escapeHtml(I18n.t('skills.curator.col_target'))}</th>
          <th>${escapeHtml(I18n.t('skills.curator.col_rationale'))}</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>
    <div class="skills-curator-foot">
      <tf-button variant="primary" id="curator-apply" ${state.curatorBusy ? 'disabled' : ''}>${escapeHtml(I18n.t('skills.curator.apply_action'))}</tf-button>
    </div>
  `;

  host.querySelectorAll('tf-checkbox[data-action-idx]').forEach((cb) => {
    cb.addEventListener('change', (e) => {
      const idx = Number(cb.getAttribute('data-action-idx'));
      if (e.detail?.checked) state.curatorApproved.add(idx);
      else state.curatorApproved.delete(idx);
    });
  });
  byId('curator-apply')?.addEventListener('click', () => applyCurator());
}
