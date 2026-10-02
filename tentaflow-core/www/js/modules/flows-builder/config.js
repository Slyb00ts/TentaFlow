// =============================================================================
// Plik: modules/flows-builder/config.js
// Opis: Panel konfiguracji wybranego node'a w Flow Builderze. Generuje
//       formularz na podstawie params_schema z template, zakładki
//       (Konfiguracja/Porty/Zaawansowane), preview JSON, akcje Duplikuj/Usuń.
// =============================================================================

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { getNodeName, getNodeDisplayTitle, isAutoNodeLabel } from '/js/modules/flows-builder/node-i18n.js';
import { nodeIconId, nodeColorVar } from '/js/modules/flows-builder/node-visuals.js';
import '/js/components/tf-input.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-select.js';
import '/js/components/tf-keyvalue-editor.js';
import '/js/components/tf-person-picker.js';
import '/js/components/tf-tabs.js';
import { processNodeKind } from './bpmn.js';

// Hardcoded prompt/config fields per harness node type (Part 4-5). Backend reads
// these from `node.config`; an empty value means "use the built-in default".
// Schema-driven fields (params_schema) still render via `_renderField`; these
// supplement node types whose prompts are not surfaced in the template schema.
// `textarea` → tf-textarea, `number` → tf-input[type=number], `bool` → tf-toggle.
const HARNESS_FIELDS = {
  agent_context: [
    { key: 'skills_template', kind: 'textarea', rows: 4, defaultHint: true },
    { key: 'anti_injection_note', kind: 'textarea', rows: 3, defaultHint: true },
    { key: 'delegated_results_template', kind: 'textarea', rows: 4, defaultHint: true },
  ],
  compact_context: [
    { key: 'summary_system_prompt', kind: 'textarea', rows: 4, defaultHint: true },
    { key: 'update_system_prompt', kind: 'textarea', rows: 4, defaultHint: true },
    { key: 'summary_prefix', kind: 'textarea', rows: 2, defaultHint: true },
    { key: 'summary_suffix', kind: 'textarea', rows: 2, defaultHint: true },
  ],
  agent_router: [
    { key: 'system_prompt', kind: 'textarea', rows: 5, defaultHint: true },
  ],
  spawn: [
    { key: 'agent_id', kind: 'text' },
    { key: 'agent_name', kind: 'text' },
    { key: 'task', kind: 'textarea', rows: 3 },
    { key: 'output_variable', kind: 'text' },
  ],
  await_subagents: [
    { key: 'run_ids_var', kind: 'text' },
    { key: 'timeout_secs', kind: 'number', min: 1, step: 1, placeholder: '300' },
    { key: 'mode', kind: 'enum', options: ['all', 'any'] },
  ],
  subagent_status: [
    { key: 'output_variable', kind: 'text' },
  ],
  interval: [
    { key: 'seconds', kind: 'number', min: 1, step: 1, placeholder: '10' },
  ],
  persist_turn: [
    { key: 'session_id', kind: 'text' },
  ],
};

// Region-level loop config (loop_max_iterations, loop_final_pass). These live on
// the region-ENTRY node — the target of the region's `loop_back` edge — regardless
// of its node_type (e.g. a seeded "Agent Run" uses `compact_context` as the entry,
// not a node typed `loop`). The backend reads them off the entry node (cache.rs).
const LOOP_REGION_FIELDS = [
  { key: 'loop_max_iterations', kind: 'number', min: 1, max: 100, step: 1, placeholder: '25' },
  { key: 'loop_final_pass', kind: 'bool' },
];

// Wezly wizyjne (FAZA 6, CV przez executor) nie maja params_schema w
// flow_node_templates — pole `alias` renderujemy tutaj jako SELECT zasilany
// katalogiem serwisow (surface `camera_cv`). Backend czyta `node.config["alias"]`;
// pusta wartosc oznacza "uzyj domyslnego aliasu" (defaultAlias, vision_impl.rs).
const VISION_ALIAS_FIELDS = {
  vision_classify: { key: 'alias', defaultAlias: 'tentavision-action' },
  vision_ocr: { key: 'alias', defaultAlias: 'tentavision-ocr' },
};

// Seedowane aliasy CV (seed_camera_cv_aliases) — fallback dropdowna gdy
// katalog jest chwilowo niedostepny albo pusty.
const CV_SEED_ALIASES = [
  'tentavision-detect',
  'tentavision-stan',
  'tentavision-ocr',
  'tentavision-action',
];

// Cache dla dynamic_enum dropdown opcji. Klucz `<source>:<category>`. Wartosc
// to Promise<Array<{value,label}>> — pojedynczy fetch na cala sesje GUI.
// Inwalidacja po przeladowaniu strony (Ctrl+Shift+R) — zmiana modeli/aliasow
// w sesji wymaga reload zeby builder zobaczyl nowe wpisy w dropdownie.
const _dynamicEnumCache = new Map();

async function loadDynamicEnumOptions(source, category) {
  const key = `${source}:${category || '_all'}`;
  if (_dynamicEnumCache.has(key)) return _dynamicEnumCache.get(key);
  const promise = (async () => {
    if (source === 'models') {
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
      const cat = (category || '').toLowerCase();
      const filtered = cat
        ? models.filter((m) => (m.category || '').toLowerCase() === cat)
        : models;
      const opts = filtered.map((m) => {
        const value = m.model_name || m.modelName || '';
        const display = m.display_name || m.displayName || value;
        const engine = m.engine_id || m.engineId;
        const label = engine ? `${display} (${engine})` : display;
        return { value, label };
      });
      // Aliasy ktore kieruja do modelu z tej kategorii — uzytkownik widzi je
      // pod prawdziwymi modelami z prefixem `↪`.
      for (const a of aliases) {
        if (a.is_active === false || a.isActive === false) continue;
        const target = a.target_model || a.targetModel;
        const targetModel = target ? modelByName.get(target) : null;
        if (!targetModel) continue;
        if (cat && (targetModel.category || '').toLowerCase() !== cat) continue;
        opts.push({
          value: a.alias,
          label: `↪ ${a.alias} → ${target}`,
        });
      }
      return opts;
    }
    if (source === 'cv_services') {
      // Zunifikowany katalog (serwisy + aliasy + flow) zawezony klientowo do
      // surface `camera_cv` — ten sam endpoint co zakladka Models w Services.
      const entries = await ApiBinary.list('catalogListRequest', { arrayKey: 'entries' }).catch(() => []);
      const cv = (Array.isArray(entries) ? entries : []).filter((e) => {
        const surfaces = e?.serviceSurfaces || e?.service_surfaces || [];
        return Array.isArray(surfaces) && surfaces.includes('camera_cv');
      });
      // Aliasy najpierw (z celem po strzalce, jak przy modelach), potem
      // bezposrednie wpisy serwisow/flow.
      const aliasOpts = [];
      const directOpts = [];
      for (const e of cv) {
        if (!e?.id) continue;
        const kindWrapper = e.kind || {};
        if (kindWrapper.kind === 'alias') {
          const target = kindWrapper.target || '';
          aliasOpts.push({ value: e.id, label: target ? `↪ ${e.id} → ${target}` : e.id });
        } else {
          directOpts.push({ value: e.id, label: e.id });
        }
      }
      const opts = [...aliasOpts, ...directOpts];
      if (opts.length) return opts;
      return CV_SEED_ALIASES.map((a) => ({ value: a, label: a }));
    }
    if (source === 'prompts') {
      const list = await ApiBinary.list('promptListRequest', { arrayKey: 'prompts' }).catch(() => []);
      return (Array.isArray(list) ? list : []).map((p) => ({
        value: p.id || p.promptId || '',
        label: p.name || p.id || '',
      }));
    }
    if (source === 'flows') {
      // Sub Flow picker (Harness §3.5 block 8): only active flows. The current
      // flow id is not available to this loader yet, so self-reference is not
      // filtered here — the subflow runtime guard rejects a self/cycle
      // reference (UI gap noted in the adapter).
      const list = await ApiBinary.list('flowListRequest').catch(() => []);
      return (Array.isArray(list) ? list : [])
        .filter((f) => (f.status || (f.enabled ? 'active' : 'draft')) === 'active')
        .map((f) => ({
          value: f.id || f.flowId || '',
          label: f.name || f.id || '',
        }));
    }
    if (source === 'agents') {
      // Agent picker (Harness §3.5 blocks 3/6/7): only enabled agents. Used by
      // agent_context, agent and agent_router config. The agents list response
      // carries a JSON string (agentsJson), not a structured array, so parse it
      // here. The value is the agent id; the label prefers display_name.
      const resp = await ApiBinary.one('agentsListRequest', {}).catch(() => null);
      let rows = [];
      try {
        rows = JSON.parse(resp?.agentsJson ?? resp?.agents_json ?? '[]');
      } catch (_e) {
        rows = [];
      }
      return (Array.isArray(rows) ? rows : [])
        .filter((a) => a.is_enabled !== false && a.isEnabled !== false)
        .map((a) => ({
          value: a.id || a.agentId || '',
          label: a.display_name || a.displayName || a.name || a.id || '',
        }));
    }
    if (source === 'projects') {
      // Project picker (project_knowledge node): active projects only. The
      // response is already filtered server-side to projects the caller is a
      // member of (plus admin visibility), so no client-side ACL is applied.
      const resp = await ApiBinary.one('projectStudioProjectsListRequest', {
        includeArchived: false,
      }).catch(() => null);
      const rows = Array.isArray(resp?.projects) ? resp.projects : [];
      return rows.map((p) => ({
        value: p.project_id || p.projectId || '',
        label: p.name || p.project_id || p.projectId || '',
      }));
    }
    if (source === 'bus_instances') {
      // TentaBus instance picker (`bus_publish`/`bus_consume`/`bus_transform`,
      // SUM/tentabus/PLAN-APP-PLATFORM.md §3.3) — reuses the same unified
      // `appsListRequest` roster the sidebar/apps-home tiles are built from
      // (`app.js:426`), narrowed to this one package. Only ENABLED instances
      // are offered: a disabled one is not addressable (`app_gate`'s
      // `AppUnavailable`), and an already-saved reference to one that later
      // got disabled or uninstalled surfaces through the generic "stale
      // value" branch above (`flows_config.dynamic_stale`) instead of a
      // bespoke error state here.
      const apps = await ApiBinary.list('appsListRequest', { arrayKey: 'apps' }).catch(() => []);
      return (Array.isArray(apps) ? apps : [])
        .filter((a) => (a.packageId ?? a.package_id) === 'tentabus' && a.enabled !== false)
        .map((a) => ({
          value: String(a.addonId ?? a.addon_id ?? ''),
          label: String((a.titleKey && I18n.t(a.titleKey)) || a.title || a.addonId || a.addon_id || ''),
        }))
        .filter((o) => o.value);
    }
    return [];
  })();
  _dynamicEnumCache.set(key, promise);
  return promise;
}

export class FlowConfig {
  constructor(rootEl, opts = {}) {
    this.root = rootEl;
    this.opts = opts;
    this.node = null;
    this.template = null;
    this.activeTab = 'config';
    this.readOnly = !!opts.readOnly;
    this.mode = opts.mode || 'flow';
    this.root.classList.add('fb-config');
    this.renderEmpty();
  }

  setTemplate(tpl) { this.template = tpl; }

  show(node, template) {
    this.node = node;
    this.template = template;
    this.activeTab = 'config';
    if (!node) { this.renderEmpty(); return; }
    if (this.mode === 'bpmn') this._renderProcess();
    else this._render();
  }

  showEdge(edge) {
    this.node = null;
    this.edge = edge;
    if (!edge) { this.renderEmpty(); return; }
    const canvas = this.opts.getCanvas();
    const source = canvas.nodes.find((node) => node.id === edge.from_node);
    const target = canvas.nodes.find((node) => node.id === edge.to_node);
    const name = (node) => node?.label || getNodeName(node?.type);
    const exclusive = source?.type === 'bpmn_exclusive_gateway';
    this.root.innerHTML = `
      <div class="fb-config-header"><div class="fb-config-title-wrap">
        <div class="fb-config-title">${escapeHtml(I18n.t('bpmn.sequence_flow'))}</div>
        <div class="fb-config-subtitle">${escapeHtml(name(source))} → ${escapeHtml(name(target))}</div>
      </div></div>
      <div class="fb-config-body">
        <tf-input data-process="condition" label="${escapeAttr(I18n.t('bpmn.condition'))}" hint="${escapeAttr(I18n.t('bpmn.condition_hint'))}" value="${escapeAttr(edge.condition || '')}" ${this.readOnly ? 'disabled' : ''}></tf-input>
        ${exclusive ? `<tf-toggle data-process="default" label="${escapeAttr(I18n.t('bpmn.default_path'))}" ${source.config.defaultFlowId === edge.id ? 'checked' : ''} ${this.readOnly ? 'disabled' : ''}></tf-toggle>` : ''}
        <tf-input readonly label="${escapeAttr(I18n.t('bpmn.element_id'))}" value="${escapeAttr(edge.id)}"></tf-input>
      </div>
      ${this.readOnly ? '' : `<footer class="fb-config-footer"><tf-button variant="danger" icon="trash" data-process="delete">${escapeHtml(I18n.t('flows_config.delete'))}</tf-button></footer>`}`;
    this.root.querySelector('[data-process="condition"]').addEventListener('change', (event) => {
      if (event.target.tagName !== 'TF-INPUT' || this.readOnly) return;
      this.opts.onEdgeChange(edge.id, { condition: event.target.value.trim() || null });
    });
    this.root.querySelector('[data-process="default"]')?.addEventListener('change', (event) => {
      if (event.target.tagName !== 'TF-TOGGLE' || this.readOnly) return;
      this.opts.onConfigChange(source.id, { defaultFlowId: event.detail.checked ? edge.id : null });
    });
    this.root.querySelector('[data-process="delete"]')?.addEventListener('click', () => {
      canvas.selectedEdgeId = edge.id;
      canvas.deleteSelected();
      this.renderEmpty();
    });
  }

  _renderProcess() {
    const node = this.node;
    const kind = processNodeKind(node.type);
    const config = node.config;
    const disabled = this.readOnly ? 'disabled' : '';
    const input = (key, label, value, attrs = '') => `<tf-input data-process="${key}" label="${escapeAttr(I18n.t(`bpmn.${label}`))}" value="${escapeAttr(String(value ?? ''))}" ${attrs} ${disabled}></tf-input>`;
    const mapping = (key, label) => `<tf-keyvalue-editor data-process="${key}" label="${escapeAttr(I18n.t(`bpmn.${label}`))}" add-label="${escapeAttr(I18n.t('bpmn.add_mapping'))}" remove-label="${escapeAttr(I18n.t('bpmn.remove_mapping'))}" key-placeholder="${escapeAttr(I18n.t('bpmn.mapping_key'))}" value-placeholder="${escapeAttr(I18n.t('bpmn.mapping_expression'))}" ${disabled}></tf-keyvalue-editor>`;
    let fields = `<tf-textarea data-process="name" label="${escapeAttr(I18n.t('bpmn.element_name'))}" value="${escapeAttr(node.label || '')}" autogrow rows="1" ${disabled}></tf-textarea>`;
    if (kind === 'UserTask') {
      const available = !config.assigneeUserId || this.opts.processOptions.assignees.some((user) => user.userId === config.assigneeUserId);
      fields += `<tf-person-picker data-process="assigneeUserId" label="${escapeAttr(I18n.t('bpmn.assignee'))}" ${this.readOnly ? 'inert' : ''}></tf-person-picker>
        ${available ? '' : `<tf-alert tone="warning" message="${escapeAttr(I18n.t('bpmn.assignee_unavailable'))}"></tf-alert>`}
        ${this.readOnly ? '' : `<tf-button variant="secondary" data-initiator>${escapeHtml(I18n.t('bpmn.use_initiator'))}</tf-button>`}
        <p class="fb-field-hint" data-assignee-hint>${escapeHtml(I18n.t(config.assigneeUserId ? 'bpmn.assignee_hint' : 'bpmn.initiator'))}</p>${mapping('outputMapping', 'output_mapping')}`;
    } else if (kind === 'ServiceTask') {
      const flows = this.opts.processOptions.serviceFlows;
      const options = `<option value="">${escapeHtml(I18n.t('bpmn.choose_flow'))}</option>` + flows.map((flow) => `<option value="${escapeAttr(flow.flowId)}">${escapeHtml(flow.name)}</option>`).join('');
      const unavailable = config.flowId && !flows.some((flow) => flow.flowId === config.flowId);
      fields += `<tf-select data-process="flowId" label="${escapeAttr(I18n.t('bpmn.service_flow'))}" value="${escapeAttr(config.flowId)}" ${disabled}>${options}${unavailable ? `<option value="${escapeAttr(config.flowId)}" disabled>${escapeHtml(I18n.t('bpmn.flow_unavailable'))}</option>` : ''}</tf-select>
        ${mapping('inputMapping', 'input_mapping')}${mapping('outputMapping', 'output_mapping')}
        <tf-select data-process="verification" label="${escapeAttr(I18n.t('bpmn.verification'))}" value="${typeof config.verification === 'string' ? 'Human' : 'Condition'}" ${disabled}>
          <option value="Human">${escapeHtml(I18n.t('bpmn.verification_human'))}</option>
          <option value="Condition">${escapeHtml(I18n.t('bpmn.verification_condition'))}</option>
        </tf-select>
        <div data-process-condition ${typeof config.verification === 'string' ? 'hidden' : ''}>${input('expression', 'verification_expression', config.verification?.Condition?.expression)}</div>
        <p class="fb-field-hint">${escapeHtml(I18n.t('bpmn.verification_hint'))}</p>
        ${input('timeoutSeconds', 'timeout', config.timeoutSeconds, 'type="number" min="1" max="600" step="1"')}`;
    } else if (kind === 'TimerStart' || kind === 'TimerCatch' || kind === 'BoundaryTimer') {
      if (kind === 'BoundaryTimer') {
        const activities = this.opts.getCanvas().nodes.filter((candidate) => ['bpmn_user_task', 'bpmn_service_task'].includes(candidate.type));
        const target = activities.find((candidate) => candidate.id === config.attachedToId);
        fields += `<tf-select data-process="attachedToId" label="${escapeAttr(I18n.t('bpmn.boundary_attach'))}" value="${escapeAttr(config.attachedToId || '')}" ${disabled}>
          <option value="">${escapeHtml(I18n.t('bpmn.boundary_choose_activity'))}</option>
          ${activities.map((candidate) => `<option value="${escapeAttr(candidate.id)}">${escapeHtml(candidate.label || getNodeName(candidate.type))}</option>`).join('')}
          ${config.attachedToId && !target ? `<option value="${escapeAttr(config.attachedToId)}" disabled>${escapeHtml(I18n.t('bpmn.boundary_unavailable'))}</option>` : ''}
        </tf-select><p class="fb-field-hint" data-boundary-target>${escapeHtml(target ? (target.label || getNodeName(target.type)) : I18n.t(config.attachedToId ? 'bpmn.boundary_unavailable' : 'bpmn.boundary_required'))}</p>
        <tf-toggle data-process="cancelActivity" label="${escapeAttr(I18n.t('bpmn.boundary_interrupting'))}" ${config.cancelActivity ? 'checked' : ''} ${disabled}></tf-toggle>
        <p class="fb-field-hint">${escapeHtml(I18n.t('bpmn.boundary_mode_hint'))}</p>`;
      }
      const timerKind = Object.keys(config.timer)[0];
      const timer = config.timer[timerKind];
      const types = kind === 'TimerStart' ? ['Date', 'Duration', 'Cycle', 'Daily'] : ['Date', 'Duration'];
      fields += `<tf-select data-process="timerType" label="${escapeAttr(I18n.t('bpmn.timer_type'))}" value="${escapeAttr(timerKind)}" ${disabled}>${types.map((type) => `<option value="${type}">${escapeHtml(I18n.t(`bpmn.timer_type_${type.toLowerCase()}`))}</option>`).join('')}</tf-select>`;
      if (timerKind === 'Date') fields += input('timerAt', 'timer_at', timer.at, 'placeholder="2026-10-02T15:30:00+02:00"') + `<p class="fb-field-hint">${escapeHtml(I18n.t('bpmn.timer_date_hint'))}</p><p class="fb-timer-literal" data-timer-literal>${escapeHtml(timer.at)}</p>`;
      else if (timerKind === 'Duration' || timerKind === 'Cycle') fields += input('timerSeconds', 'timer_seconds', timer.seconds, `type="number" min="${timerKind === 'Cycle' ? 300 : 1}" max="31536000" step="1"`) + `<p class="fb-field-hint">${escapeHtml(I18n.t(timerKind === 'Cycle' ? 'bpmn.timer_cycle_hint' : 'bpmn.timer_duration_hint'))}</p>`;
      else if (timerKind === 'Daily') fields += `<div class="fb-timer-clock">${input('timerHour', 'timer_hour', timer.hour, 'type="number" min="0" max="23" step="1"')}${input('timerMinute', 'timer_minute', timer.minute, 'type="number" min="0" max="59" step="1"')}</div><p class="fb-field-hint">${escapeHtml(I18n.t('bpmn.timer_daily_hint'))}</p>`;
      if (timerKind === 'Cycle' || timerKind === 'Daily') fields += input('timerTotal', 'timer_total', timer.totalFirings, 'type="number" min="1" max="4294967295" step="1"') + `<p class="fb-field-hint">${escapeHtml(I18n.t('bpmn.timer_total_hint'))}</p>`;
      fields += `<p class="fb-field-hint">${escapeHtml(I18n.t(kind === 'TimerStart' ? 'bpmn.timer_start_hint' : kind === 'BoundaryTimer' ? 'bpmn.boundary_timer_hint' : 'bpmn.timer_catch_hint'))}</p>`;
    } else if (kind === 'ExclusiveGateway') {
      const canvas = this.opts.getCanvas();
      const outgoing = canvas.edges.filter((edge) => edge.from_node === node.id);
      fields += `<tf-select data-process="defaultFlowId" label="${escapeAttr(I18n.t('bpmn.default_path'))}" value="${escapeAttr(config.defaultFlowId || '')}" ${disabled}>
        <option value="">${escapeHtml(I18n.t('bpmn.no_default'))}</option>
        ${outgoing.map((edge) => {
          const target = canvas.nodes.find((candidate) => candidate.id === edge.to_node);
          return `<option value="${escapeAttr(edge.id)}">${escapeHtml(target?.label || getNodeName(target?.type))}${edge.condition ? ` — ${escapeHtml(edge.condition)}` : ''}</option>`;
        }).join('')}</tf-select><p class="fb-field-hint">${escapeHtml(I18n.t('bpmn.xor_hint'))}</p>`;
    } else {
      fields += `<p class="fb-field-hint">${escapeHtml(getNodeName(node.type))}: ${escapeHtml(I18n.t(`bpmn.node_${node.type.slice(5)}_hint`))}</p>`;
    }
    const canvas = this.opts.getCanvas();
    const outgoing = canvas.edges.filter((edge) => edge.from_node === node.id);
    if (outgoing.length) fields += `<tf-select data-process="editSequence" label="${escapeAttr(I18n.t('bpmn.edit_sequence'))}"><option value="">${escapeHtml(I18n.t('bpmn.choose_sequence'))}</option>${outgoing.map((edge) => {
      const target = canvas.nodes.find((candidate) => candidate.id === edge.to_node);
      return `<option value="${escapeAttr(edge.id)}">${escapeHtml(target.label || getNodeName(target.type))}</option>`;
    }).join('')}</tf-select>`;
    if (kind !== 'End' && !this.readOnly) fields += `<div class="fb-process-connect"><tf-select data-connect label="${escapeAttr(I18n.t('bpmn.connect_to'))}"><option value="">${escapeHtml(I18n.t('bpmn.choose_element'))}</option>${canvas.nodes.filter((target) => target.id !== node.id && !['bpmn_start', 'bpmn_timer_start', 'bpmn_boundary_timer'].includes(target.type)).map((target) => `<option value="${escapeAttr(target.id)}">${escapeHtml(target.label || getNodeName(target.type))}</option>`).join('')}</tf-select><tf-button variant="secondary" data-connect-add disabled>${escapeHtml(I18n.t('bpmn.add_sequence'))}</tf-button></div>`;
    fields += input('elementId', 'element_id', node.id, 'readonly');
    this.root.innerHTML = `<div class="fb-config-header"><div class="fb-config-title-wrap"><div class="fb-config-title">${escapeHtml(getNodeDisplayTitle(node, this.template))}</div><div class="fb-config-subtitle">${escapeHtml(getNodeName(node.type))}</div></div></div>
      <div class="fb-config-body fb-process-fields">${fields}<p class="fb-field-hint">${escapeHtml(I18n.t('bpmn.mapping_hint'))}</p></div>
      ${this.readOnly ? '' : `<footer class="fb-config-footer"><tf-button variant="secondary" icon="copy" data-process="duplicate">${escapeHtml(I18n.t('flows_config.duplicate'))}</tf-button><tf-button variant="danger" icon="trash" data-process="delete">${escapeHtml(I18n.t('flows_config.delete'))}</tf-button></footer>`}`;
    this.root.querySelectorAll('tf-keyvalue-editor').forEach((editor) => { editor.value = config[editor.dataset.process]; });
    const picker = this.root.querySelector('tf-person-picker');
    if (picker) {
      picker.items = this.opts.processOptions.assignees.map((user) => ({ id: user.userId, name: user.displayName }));
      picker.value = config.assigneeUserId || '';
      this.root.querySelector('[data-initiator]')?.addEventListener('click', () => {
        this.opts.onConfigChange(node.id, { assigneeUserId: null });
        picker.value = null;
        this.root.querySelector('[data-assignee-hint]').textContent = I18n.t('bpmn.initiator');
      });
    }
    this.root.querySelector('.fb-process-fields').addEventListener('change', (event) => {
      const control = event.target.closest('[data-process]');
      if (!control || control !== event.target || !this.root.contains(control)) return;
      const key = control.dataset.process;
      if (key === 'editSequence') { canvas.selectEdge(control.value); return; }
      if (this.readOnly) return;
      if (key === 'name') this.opts.onLabelChange(node.id, control.value);
      else if (key === 'cancelActivity') {
        this.opts.onConfigChange(node.id, { cancelActivity: event.detail?.checked ?? control.checked });
      } else if (key === 'attachedToId') {
        this.opts.onConfigChange(node.id, { attachedToId: control.value || null });
        const selected = canvas.nodes.find((candidate) => candidate.id === control.value);
        this.root.querySelector('[data-boundary-target]').textContent = selected
          ? (selected.label || getNodeName(selected.type)) : I18n.t('bpmn.boundary_required');
      } else if (key === 'verification') {
        this.opts.onConfigChange(node.id, { verification: control.value === 'Human' ? 'Human' : { Condition: { expression: this.root.querySelector('[data-process="expression"]').value } } });
        this.root.querySelector('[data-process-condition]').hidden = control.value === 'Human';
      } else if (key === 'timerType') {
        const values = { Date: { at: '' }, Duration: { seconds: 60 }, Cycle: { seconds: 300, totalFirings: null }, Daily: { hour: 9, minute: 0, totalFirings: null } };
        this.opts.onConfigChange(node.id, { timer: { [control.value]: values[control.value] } });
        this._renderProcess();
        this.root.querySelector('[data-process="timerType"]').focus();
      } else if (key.startsWith('timer')) {
        const timerKind = Object.keys(node.config.timer)[0];
        const field = { timerAt: 'at', timerSeconds: 'seconds', timerHour: 'hour', timerMinute: 'minute', timerTotal: 'totalFirings' }[key];
        const value = key === 'timerAt' ? control.value : (control.value.trim() === '' ? null : Number(control.value));
        this.opts.onConfigChange(node.id, { timer: { [timerKind]: { ...node.config.timer[timerKind], [field]: value } } });
        if (key === 'timerAt') this.root.querySelector('[data-timer-literal]').textContent = control.value;
      } else if (key === 'expression') this.opts.onConfigChange(node.id, { verification: { Condition: { expression: control.value } } });
      else if (key !== 'elementId') this.opts.onConfigChange(node.id, { [key]: key === 'timeoutSeconds' ? Number(control.value) : (control.value || (key === 'assigneeUserId' || key === 'defaultFlowId' ? null : '')) });
      if (key === 'assigneeUserId') this.root.querySelector('[data-assignee-hint]').textContent = I18n.t(control.value ? 'bpmn.assignee_hint' : 'bpmn.initiator');
    });
    const connect = this.root.querySelector('[data-connect]');
    const add = this.root.querySelector('[data-connect-add]');
    connect?.addEventListener('change', (event) => {
      if (event.target !== connect) return;
      add.toggleAttribute('disabled', !connect.value);
    });
    add?.addEventListener('click', () => {
      if (!canvas.connectNodes(node.id, connect.value)) return;
      this._renderProcess();
    });
    this.root.querySelector('[data-process="delete"]')?.addEventListener('click', () => { this.opts.onDelete(node.id); this.renderEmpty(); });
    this.root.querySelector('[data-process="duplicate"]')?.addEventListener('click', () => this.opts.onDuplicate(node.id));
  }

  renderEmpty() {
    this.node = null;
    this.root.innerHTML = `
      <div class="fb-config-empty">
        <div class="fb-config-empty-icon">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></svg>
        </div>
        <h4>${escapeHtml(I18n.t('flows_config.empty_title'))}</h4>
        <p>${escapeHtml(I18n.t('flows_config.empty_hint'))}</p>
      </div>`;
  }

  _paramsSchema() {
    if (!this.template) return { properties: {}, required: [] };
    const raw = this.template.params_schema;
    if (!raw) return { properties: {}, required: [] };
    try {
      const s = typeof raw === 'string' ? JSON.parse(raw) : raw;
      return {
        properties: s.properties || {},
        required: Array.isArray(s.required) ? s.required : [],
        order: Array.isArray(s.order) ? s.order : Object.keys(s.properties || {}),
      };
    } catch (_) {
      return { properties: {}, required: [] };
    }
  }

  _render() {
    const n = this.node;
    const iconId = nodeIconId(n.type, this.template?.icon, this.template?.category);
    const varName = nodeColorVar(n.type, this.template?.category);
    const title = getNodeDisplayTitle(n, this.template);
    const subtitle = I18n.t('flows_config.subtitle', { type: n.type, id: n.id });

    this.root.innerHTML = `
      <div class="fb-config-header">
        <div class="fb-node-badge" style="--node-color: var(${varName})"><svg><use href="#i-${iconId}"/></svg></div>
        <div class="fb-config-title-wrap">
          <div class="fb-config-title">${escapeHtml(title)}</div>
          <div class="fb-config-subtitle">${escapeHtml(subtitle)}</div>
        </div>
      </div>
      <tf-tabs class="fb-config-tabs" value="${this.activeTab}" variant="underline">
        <tf-tab id="config" label="${escapeAttr(I18n.t('flows_config.tab_config'))}"></tf-tab>
        <tf-tab id="mapping" label="${escapeAttr(I18n.t('flows_config.tab_mapping'))}"></tf-tab>
        <tf-tab id="ports" label="${escapeAttr(I18n.t('flows_config.tab_ports'))}"></tf-tab>
        <tf-tab id="advanced" label="${escapeAttr(I18n.t('flows_config.tab_advanced'))}"></tf-tab>
      </tf-tabs>
      <div class="fb-config-body" data-role="body"></div>
      ${this.readOnly ? '' : `<footer class="fb-config-footer">
        <tf-button variant="secondary" size="sm" icon="copy" data-action="duplicate">${escapeHtml(I18n.t('flows_config.duplicate'))}</tf-button>
        <tf-button variant="danger" size="sm" icon="trash" data-action="delete">${escapeHtml(I18n.t('flows_config.delete'))}</tf-button>
      </footer>`}
    `;

    this.root.querySelector('tf-tabs').addEventListener('change', (event) => {
      if (event.target.tagName !== 'TF-TABS') return;
      this.activeTab = event.detail.value;
      this._renderBody();
    });

    this.root.querySelectorAll('[data-action]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const act = btn.dataset.action;
        if (act === 'delete') this.opts.onDelete?.(n.id);
        else if (act === 'duplicate') this.opts.onDuplicate?.(n.id);
      });
    });

    this._renderBody();
  }

  _renderBody() {
    const body = this.root.querySelector('[data-role="body"]');
    if (!body) return;
    if (this.activeTab === 'config') body.innerHTML = this._renderConfigTab();
    else if (this.activeTab === 'mapping') body.innerHTML = this._renderMappingTab();
    else if (this.activeTab === 'ports') body.innerHTML = this._renderPortsTab();
    else body.innerHTML = this._renderAdvancedTab();

    if (this.activeTab === 'config') this._bindConfigInputs(body);
    if (this.activeTab === 'mapping') this._bindMappingInputs(body);
    if (this.activeTab === 'advanced') this._bindAdvancedInputs(body);
    if (this.activeTab === 'ports') this._bindPortsInputs(body);
    if (this.readOnly) {
      body.querySelectorAll('input, select, textarea, button, tf-button, tf-input, tf-select, tf-textarea, tf-toggle, tf-checkbox, tf-combobox')
        .forEach((el) => el.setAttribute('disabled', ''));
    }
  }

  _bindPortsInputs(body) {
    const n = this.node;
    if (!(n.type === 'switch' || n.type === 'router')) return;
    const readCases = () => {
      const list = [];
      body.querySelectorAll('[data-bind-case]').forEach((inp) => {
        const v = (inp.value || '').trim();
        if (v) list.push(v);
      });
      return list;
    };
    body.querySelectorAll('[data-bind-case]').forEach((inp) => {
      inp.addEventListener('change', () => {
        this.opts.onConfigChange?.(n.id, { cases: readCases() });
        this._refreshPreview();
      });
    });
    const addBtn = body.querySelector('[data-action="add-case"]');
    addBtn?.addEventListener('click', () => {
      const current = readCases();
      current.push(`case_${current.length + 1}`);
      this.opts.onConfigChange?.(n.id, { cases: current });
      this._refreshPreview();
      // Re-render ports tab żeby pojawił się nowy wiersz
      this._renderBody();
    });
    body.querySelectorAll('[data-action="remove-case"]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const idx = parseInt(btn.dataset.idx, 10);
        const current = readCases();
        current.splice(idx, 1);
        this.opts.onConfigChange?.(n.id, { cases: current });
        this._refreshPreview();
        this._renderBody();
      });
    });
  }

  _renderConfigTab() {
    const n = this.node;
    const schema = this._paramsSchema();
    const props = schema.properties;
    const required = schema.required;
    const keys = schema.order && schema.order.length ? schema.order : Object.keys(props);

    const labelVal = isAutoNodeLabel(n.label, n.type, this.template?.label) ? '' : (n.label || '');
    let html = `
      <div class="fb-field">
        <tf-input data-bind="label" data-type="string" label="${escapeAttr(I18n.t('flows_config.name'))}" value="${escapeAttr(labelVal)}" placeholder="${escapeAttr(getNodeName(n.type, this.template?.label))}"></tf-input>
      </div>
    `;

    // Schema-driven config keys already covered by HARNESS_FIELDS are skipped
    // here so each prompt/field renders exactly once (harness fields win, they
    // carry the "empty → default" hint the backend expects).
    const harness = HARNESS_FIELDS[n.type] || [];
    const harnessKeys = new Set(harness.map((f) => f.key));

    for (const key of keys) {
      if (harnessKeys.has(key)) continue;
      const def = props[key];
      if (!def) continue;
      const value = n.config?.[key];
      html += this._renderField(key, def, value, required.includes(key));
    }

    for (const f of harness) {
      html += this._renderHarnessField(f, n.config?.[f.key]);
    }

    // Wezly wizyjne: wybor serwisu CV (aliasu) z katalogu zamiast recznego
    // wpisywania. Renderowane tylko gdy schema nie pokrywa juz tego klucza.
    const visionField = VISION_ALIAS_FIELDS[n.type];
    if (visionField && !keys.includes(visionField.key)) {
      html += this._renderField(visionField.key, {
        type: 'string',
        title: I18n.t('flows_config.vision_alias_label'),
        description: I18n.t('flows_config.vision_alias_hint', { default: visionField.defaultAlias }),
        dynamic_enum: { source: 'cv_services' },
      }, n.config?.[visionField.key], false);
    }

    // Region-level loop config belongs to the region-ENTRY node only (target of
    // the region's loop_back edge), independent of node_type. The role is read
    // from the live graph so it tracks edits without a schema marker.
    const isRegionEntry = this.opts.getCanvas?.()?.regionRole?.(n.id) === 'entry';
    if (isRegionEntry) {
      html += `<div class="fb-field-section">${escapeHtml(I18n.t('flows_config.loop_region_section'))}</div>`;
      for (const f of LOOP_REGION_FIELDS) {
        html += this._renderHarnessField(f, n.config?.[f.key]);
      }
    }

    if (keys.length === 0 && harness.length === 0 && !visionField && !isRegionEntry) {
      html += `<div class="fb-field-hint">${escapeHtml(I18n.t('flows_config.no_params_hint'))}</div>`;
    }

    // Preview input/output
    html += `
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.preview_label'))}</label>
        <div class="fb-config-preview">${this._jsonPreview({ label: n.label, type: n.type, config: n.config })}</div>
      </div>
    `;
    return html;
  }

  _renderField(key, def, value, isRequired) {
    const type = def.type || 'string';
    const title = def.title || key;
    const hint = def.description || '';
    const curVal = value !== undefined && value !== null ? value : (def.default !== undefined ? def.default : '');
    const reqMark = isRequired ? ' *' : '';
    const labelAttr = escapeAttr(title + reqMark);
    const hintAttr = hint ? ` hint="${escapeAttr(hint)}"` : '';

    if (type === 'boolean') {
      return `
        <div class="fb-field fb-field-row">
          <div>
            <label class="fb-label">${escapeHtml(title)}${reqMark}</label>
            ${hint ? `<div class="fb-field-hint">${escapeHtml(hint)}</div>` : ''}
          </div>
          <tf-toggle data-bind="${escapeAttr(key)}" data-type="boolean" ${curVal ? 'checked' : ''}></tf-toggle>
        </div>`;
    }

    if (Array.isArray(def.enum)) {
      const opts = def.enum.map((opt) => {
        const v = typeof opt === 'object' ? opt.value : opt;
        const l = typeof opt === 'object' ? opt.label : opt;
        const sel = String(curVal) === String(v) ? 'selected' : '';
        return `<option value="${escapeAttr(v)}" ${sel}>${escapeHtml(l)}</option>`;
      }).join('');
      return `
        <div class="fb-field">
          <tf-select data-bind="${escapeAttr(key)}" data-type="string" label="${labelAttr}"${hintAttr} value="${escapeAttr(String(curVal))}">${opts}</tf-select>
        </div>`;
    }

    if (def.dynamic_enum && typeof def.dynamic_enum === 'object') {
      // Renderujemy placeholder tf-select; opcje zaciagamy async po renderze
      // (loadDynamicEnumOptions z cache). Aktualna wartosc trzymana jako
      // jedyna opcja zeby preview JSON pokazywal poprawnie.
      const source = String(def.dynamic_enum.source || '');
      const category = String(def.dynamic_enum.category || '');
      const placeholder = curVal ? escapeHtml(String(curVal)) : escapeHtml(I18n.t('flows_config.select_placeholder'));
      return `
        <div class="fb-field">
          <tf-select data-bind="${escapeAttr(key)}" data-type="string" label="${labelAttr}"${hintAttr}
                  data-dynamic-source="${escapeAttr(source)}"
                  data-dynamic-category="${escapeAttr(category)}" value="${escapeAttr(curVal || '')}">
            <option value="${escapeAttr(curVal || '')}" selected>${placeholder}</option>
          </tf-select>
        </div>`;
    }

    if (type === 'number' || type === 'integer') {
      const step = def.step || (type === 'integer' ? 1 : 'any');
      const rangeAttrs = `${def.minimum != null ? ` min="${def.minimum}"` : ''}${def.maximum != null ? ` max="${def.maximum}"` : ''} step="${step}"`;
      return `
        <div class="fb-field">
          <tf-input type="number" data-bind="${escapeAttr(key)}" data-type="number" label="${labelAttr}"${hintAttr}${rangeAttrs} value="${escapeAttr(String(curVal))}"></tf-input>
        </div>`;
    }

    if (type === 'object') {
      // Scalar-valued map (JSON-schema `additionalProperties` of a simple
      // type, e.g. bus_publish's `headers`). The editor's own initial
      // `.value` is set in `_bindConfigInputs` (an object cannot round-trip
      // through an HTML attribute), so this arm only lays out the tag.
      return `
        <div class="fb-field">
          <label class="fb-label">${escapeHtml(title)}${reqMark}</label>
          ${hint ? `<div class="fb-field-hint">${escapeHtml(hint)}</div>` : ''}
          <tf-keyvalue-editor data-bind="${escapeAttr(key)}" data-type="object"
            key-placeholder="${escapeAttr(I18n.t('settings.key'))}"
            value-placeholder="${escapeAttr(I18n.t('settings.value'))}"
            add-label="${escapeAttr(I18n.t('flows_config.map_add'))}"
            remove-label="${escapeAttr(I18n.t('flows_config.map_remove'))}"></tf-keyvalue-editor>
        </div>`;
    }

    if (def.format === 'textarea' || (typeof curVal === 'string' && curVal.length > 80)) {
      return `
        <div class="fb-field">
          <tf-textarea data-bind="${escapeAttr(key)}" data-type="string" label="${labelAttr}"${hintAttr} rows="4" placeholder="${escapeAttr(def.placeholder || '')}" value="${escapeAttr(String(curVal))}"></tf-textarea>
        </div>`;
    }

    return `
      <div class="fb-field">
        <tf-input type="text" data-bind="${escapeAttr(key)}" data-type="string" label="${labelAttr}"${hintAttr} value="${escapeAttr(String(curVal))}" placeholder="${escapeAttr(def.placeholder || '')}"></tf-input>
      </div>`;
  }

  // Renders a hardcoded harness field (prompts / loop region / background block
  // params from Part 4-5) through tf-* primitives. `defaultHint` shows the
  // "empty → built-in default" note the backend relies on.
  _renderHarnessField(f, value) {
    const title = I18n.t(`flows_config.harness.${f.key}`);
    const curVal = value !== undefined && value !== null ? value : '';
    const hint = f.defaultHint ? ` hint="${escapeAttr(I18n.t('flows_config.harness_default_hint'))}"` : '';
    const labelAttr = escapeAttr(title);

    if (f.kind === 'bool') {
      return `
        <div class="fb-field fb-field-row">
          <div>
            <label class="fb-label">${escapeHtml(title)}</label>
            ${f.defaultHint ? `<div class="fb-field-hint">${escapeHtml(I18n.t('flows_config.harness_default_hint'))}</div>` : ''}
          </div>
          <tf-toggle data-bind="${escapeAttr(f.key)}" data-type="boolean" ${curVal ? 'checked' : ''}></tf-toggle>
        </div>`;
    }
    if (f.kind === 'number') {
      const rangeAttrs = `${f.min != null ? ` min="${f.min}"` : ''}${f.max != null ? ` max="${f.max}"` : ''}${f.step != null ? ` step="${f.step}"` : ''}`;
      return `
        <div class="fb-field">
          <tf-input type="number" data-bind="${escapeAttr(f.key)}" data-type="number" label="${labelAttr}"${hint}${rangeAttrs} value="${escapeAttr(String(curVal))}" placeholder="${escapeAttr(f.placeholder || '')}"></tf-input>
        </div>`;
    }
    if (f.kind === 'enum') {
      const opts = (f.options || []).map((opt) => {
        const sel = String(curVal) === String(opt) ? 'selected' : '';
        return `<option value="${escapeAttr(opt)}" ${sel}>${escapeHtml(opt)}</option>`;
      }).join('');
      return `
        <div class="fb-field">
          <tf-select data-bind="${escapeAttr(f.key)}" data-type="string" label="${labelAttr}"${hint} value="${escapeAttr(String(curVal))}">${opts}</tf-select>
        </div>`;
    }
    if (f.kind === 'textarea') {
      return `
        <div class="fb-field">
          <tf-textarea data-bind="${escapeAttr(f.key)}" data-type="string" label="${labelAttr}"${hint} rows="${f.rows || 3}" placeholder="${escapeAttr(f.placeholder || '')}" value="${escapeAttr(String(curVal))}"></tf-textarea>
        </div>`;
    }
    return `
      <div class="fb-field">
        <tf-input type="text" data-bind="${escapeAttr(f.key)}" data-type="string" label="${labelAttr}"${hint} value="${escapeAttr(String(curVal))}" placeholder="${escapeAttr(f.placeholder || '')}"></tf-input>
      </div>`;
  }

  _bindConfigInputs(body) {
    // Captured ONCE per render, at bind time — not re-read as `this.node`
    // inside the listeners below. `this.node` is reassigned synchronously by
    // `show()` the moment a different node is selected, and switching
    // selection while a field here still has focus tears this body out of the
    // DOM, which fires that field's own `blur`/`change` on the way out. A
    // listener reading `this.node.id` at that point would commit the pending
    // edit onto the NEWLY selected node instead of the one this panel was
    // opened for.
    const node = this.node;
    body.querySelectorAll('[data-bind]').forEach((el) => {
      const key = el.dataset.bind;
      if (key === 'label') {
        // tf-input emits `change` with detail.value; read .value for both.
        el.addEventListener('change', () => {
          this.opts.onLabelChange?.(node.id, el.value);
        });
        return;
      }
      // tf-toggle reports its new state in detail.checked (no .value).
      if (el.tagName === 'TF-TOGGLE') {
        el.addEventListener('change', (e) => {
          const on = e.detail?.checked ?? el.checked;
          this.opts.onConfigChange?.(node.id, { [key]: on });
          this._refreshPreview();
        });
        return;
      }
      // tf-keyvalue-editor exposes `.value` as a plain object, not a string —
      // its initial value comes from node.config (an object cannot round-trip
      // through an HTML attribute the way `_renderField` writes the rest).
      if (el.tagName === 'TF-KEYVALUE-EDITOR') {
        const current = node.config?.[key];
        el.value = current && typeof current === 'object' ? current : {};
        el.addEventListener('change', (e) => {
          this.opts.onConfigChange?.(node.id, { [key]: e.detail?.value ?? el.value });
          this._refreshPreview();
        });
        return;
      }
      const type = el.dataset.type;
      // tf-input/tf-textarea/tf-select all expose `.value` and emit `change`.
      el.addEventListener('change', () => {
        let v = el.value;
        if (type === 'number') v = v === '' ? undefined : parseFloat(v);
        this.opts.onConfigChange?.(node.id, { [key]: v });
        this._refreshPreview();
      });
    });

    // Async populate dynamic_enum tf-select. Bierzemy aktualna wartosc z
    // node.config zeby zachowac selekcje po refresh listy. Jak fetch
    // failuje, zostawiamy single-option placeholder + log do konsoli.
    body.querySelectorAll('tf-select[data-dynamic-source]').forEach(async (sel) => {
      const source = sel.dataset.dynamicSource;
      const category = sel.dataset.dynamicCategory || '';
      const key = sel.dataset.bind;
      const currentValue = (this.node.config && this.node.config[key]) || '';
      try {
        const opts = await loadDynamicEnumOptions(source, category);
        if (!opts.length) {
          sel.setOptions([{ value: '', label: I18n.t('flows_config.dynamic_empty', { source, category }), disabled: true }], '');
          return;
        }
        const list = [{ value: '', label: I18n.t('flows_config.select_placeholder') }, ...opts];
        // Jesli aktualna wartosc nie jest na liscie (np. usuniety alias),
        // dodajemy ja jako "niedostepne" zeby user widzial ze cos przepadlo.
        if (currentValue && !opts.some((o) => String(o.value) === String(currentValue))) {
          list.push({ value: currentValue, label: I18n.t('flows_config.dynamic_stale', { value: currentValue }) });
        }
        sel.setOptions(list, currentValue || '');
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(`[fb-config] dynamic_enum load failed for ${source}:${category}:`, err);
      }
    });
  }

  // Io-mapping editor (§3.12, phase 7): CEL expressions feeding node config
  // (input_mapping) and writing flow variables (output_mapping). The backend
  // executor evaluates these around the adapter; it also rejects malformed
  // shapes and undeclared output targets (R10) on save, so the client only adds
  // a lightweight sanity hint and never reimplements CEL validation.
  _renderMappingTab() {
    const n = this.node;
    const input = this._mappingRowsFrom(n.config?.input_mapping);
    const output = this._mappingRowsFrom(n.config?.output_mapping);
    const declaredVars = this._declaredVariables();

    return `
      <div class="fb-map-section" data-mapping="input_mapping">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.map_input_label'))}</label>
        <div class="fb-field-hint">${escapeHtml(I18n.t('flows_config.map_input_hint'))}</div>
        <div class="fb-map-list" data-role="rows">${
          input.length
            ? input.map((r, i) => this._renderMappingRow('input_mapping', r, i)).join('')
            : `<div class="fb-vars-empty">${escapeHtml(I18n.t('flows_config.map_input_empty'))}</div>`
        }</div>
        <tf-button variant="secondary" size="sm" icon="plus" data-action="add-row">${escapeHtml(I18n.t('flows_config.map_add'))}</tf-button>
      </div>
      <div class="fb-map-section" data-mapping="output_mapping">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.map_output_label'))}</label>
        <div class="fb-field-hint">${escapeHtml(I18n.t('flows_config.map_output_hint'))}</div>
        ${
          declaredVars.length
            ? ''
            : `<div class="fb-field-hint fb-map-warn">${escapeHtml(I18n.t('flows_config.map_no_vars'))}</div>`
        }
        <div class="fb-map-list" data-role="rows">${
          output.length
            ? output.map((r, i) => this._renderMappingRow('output_mapping', r, i)).join('')
            : `<div class="fb-vars-empty">${escapeHtml(I18n.t('flows_config.map_output_empty'))}</div>`
        }</div>
        <tf-button variant="secondary" size="sm" icon="plus" data-action="add-row">${escapeHtml(I18n.t('flows_config.map_add'))}</tf-button>
      </div>
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.map_scope_label'))}</label>
        <div class="fb-field-hint">${escapeHtml(I18n.t('flows_config.map_scope_hint'))}</div>
      </div>
    `;
  }

  _renderMappingRow(mapping, row, idx) {
    const keyPlaceholder = mapping === 'input_mapping'
      ? I18n.t('flows_config.map_key_placeholder')
      : I18n.t('flows_config.map_var_placeholder');
    const warn = this._expressionHint(row.expression);
    return `
      <div class="fb-map-row" data-idx="${idx}">
        <tf-combobox class="fb-map-key" data-field="key" placeholder="${escapeAttr(keyPlaceholder)}" value="${escapeAttr(row.key)}"></tf-combobox>
        <span class="fb-map-arrow" aria-hidden="true">→</span>
        <tf-input class="fb-map-expr" data-field="expression" placeholder="${escapeAttr(I18n.t('flows_config.map_expr_placeholder'))}" value="${escapeAttr(row.expression)}"></tf-input>
        <tf-button variant="ghost" size="sm" icon="trash" data-action="remove-row" title="${escapeAttr(I18n.t('flows_config.map_remove'))}"></tf-button>
        <div class="fb-map-rowhint ${warn ? 'warn' : ''}" data-role="rowhint">${warn ? escapeHtml(warn) : ''}</div>
      </div>`;
  }

  // Populates each row's tf-combobox key field with suggestions (config keys for
  // input_mapping, declared variables for output_mapping). tf-combobox ingests
  // options only via the `.options` property, so it must be set after mount.
  _populateMappingKeys(sectionEl, mapping) {
    const keys = mapping === 'input_mapping'
      ? this._knownConfigKeys()
      : this._declaredVariables();
    const options = keys.map((k) => ({ value: k, label: k }));
    sectionEl.querySelectorAll('tf-combobox[data-field="key"]').forEach((cb) => {
      cb.options = options;
    });
  }

  // Converts a stored {key: "<CEL>"} object into ordered editable rows. Keeps
  // insertion order from Object.entries so re-rendering is stable.
  _mappingRowsFrom(obj) {
    if (!obj || typeof obj !== 'object') return [];
    return Object.entries(obj).map(([key, expression]) => ({
      key: String(key),
      expression: typeof expression === 'string' ? expression : '',
    }));
  }

  // Config keys the adapter declares in params_schema — suggested as
  // input_mapping targets. Free text is still allowed (combobox), because some
  // adapters read keys not surfaced in the schema.
  _knownConfigKeys() {
    const schema = this._paramsSchema();
    const props = schema.properties || {};
    return (schema.order && schema.order.length ? schema.order : Object.keys(props))
      .filter((k) => k !== 'input_mapping' && k !== 'output_mapping');
  }

  // Declared flow variables (flow_json.variables) — the only legal
  // output_mapping targets (R10). Supplied by the builder page via getFlowVariables.
  _declaredVariables() {
    const list = this.opts.getFlowVariables?.() || [];
    return (Array.isArray(list) ? list : [])
      .map((v) => v?.name)
      .filter((name) => typeof name === 'string' && name.length > 0);
  }

  // Lightweight, non-blocking sanity hint. NOT a CEL validator — the backend
  // rejects bad expressions on save with a precise message surfaced as a toast.
  // Here we only flag obviously broken input: empty, unbalanced quotes or parens.
  _expressionHint(expr) {
    const e = (expr ?? '').trim();
    if (e === '') return I18n.t('flows_config.map_hint_empty');
    let inSingle = false;
    let inDouble = false;
    let depth = 0;
    for (let i = 0; i < e.length; i += 1) {
      const c = e[i];
      if (c === "'" && !inDouble) inSingle = !inSingle;
      else if (c === '"' && !inSingle) inDouble = !inDouble;
      else if (!inSingle && !inDouble) {
        if (c === '(' || c === '[' || c === '{') depth += 1;
        else if (c === ')' || c === ']' || c === '}') depth -= 1;
        if (depth < 0) return I18n.t('flows_config.map_hint_unbalanced');
      }
    }
    if (inSingle || inDouble) return I18n.t('flows_config.map_hint_quotes');
    if (depth !== 0) return I18n.t('flows_config.map_hint_unbalanced');
    return '';
  }

  _bindMappingInputs(body) {
    const n = this.node;

    // Reads all rows of one section back into a {key: expr} object. Empty keys
    // and empty expressions are dropped so half-typed rows never reach flow_json.
    const readMapping = (sectionEl) => {
      const out = {};
      sectionEl.querySelectorAll('.fb-map-row').forEach((rowEl) => {
        const keyEl = rowEl.querySelector('[data-field="key"]');
        const exprEl = rowEl.querySelector('[data-field="expression"]');
        const key = (keyEl?.value ?? '').trim();
        const expr = (exprEl?.value ?? '').trim();
        if (key === '' || expr === '') return;
        out[key] = expr;
      });
      return out;
    };

    const commit = (sectionEl) => {
      const mapping = sectionEl.dataset.mapping;
      const obj = readMapping(sectionEl);
      // Empty object → remove the key entirely so legacy flows round-trip
      // byte-identically (backend uses skip_serializing_if on absent mappings).
      const patch = { [mapping]: Object.keys(obj).length ? obj : undefined };
      this.opts.onConfigChange?.(n.id, patch);
      this._refreshPreview();
    };

    body.querySelectorAll('.fb-map-section').forEach((sectionEl) => {
      this._populateMappingKeys(sectionEl, sectionEl.dataset.mapping);
      // `input` only updates the cheap inline hint (no canvas/history churn);
      // `change` (blur/commit) persists into node.config — matches the switch
      // cases editor and avoids pushing history on every keystroke.
      sectionEl.addEventListener('input', (ev) => {
        const rowEl = ev.target.closest('.fb-map-row');
        if (rowEl && ev.target.dataset.field === 'expression') {
          const hintEl = rowEl.querySelector('[data-role="rowhint"]');
          const warn = this._expressionHint(ev.target.value);
          if (hintEl) {
            hintEl.textContent = warn;
            hintEl.classList.toggle('warn', Boolean(warn));
          }
        }
      });
      sectionEl.addEventListener('change', () => commit(sectionEl));
      // tf-combobox only emits `change` when a suggestion is picked, not after
      // free typing; `focusout` guarantees a free-typed key gets persisted when
      // the field loses focus.
      sectionEl.addEventListener('focusout', () => commit(sectionEl));

      sectionEl.querySelector('[data-action="add-row"]')?.addEventListener('click', () => {
        const mapping = sectionEl.dataset.mapping;
        // Current visible rows (including half-typed ones) plus one fresh empty
        // row; reading from the DOM keeps in-flight edits during the re-render.
        const rows = this._readMappingRows(sectionEl).concat([{ key: '', expression: '' }]);
        this._renderMappingSection(sectionEl, mapping, rows);
      });

      sectionEl.addEventListener('click', (ev) => {
        const btn = ev.target.closest('[data-action="remove-row"]');
        if (!btn) return;
        const rowEl = btn.closest('.fb-map-row');
        rowEl?.remove();
        commit(sectionEl);
        const mapping = sectionEl.dataset.mapping;
        this._renderMappingSection(sectionEl, mapping, this._readMappingRows(sectionEl));
      });
    });
  }

  // Reads visible rows of a section as ordered {key, expression} pairs,
  // including half-typed ones (so a re-render after add/remove keeps edits).
  _readMappingRows(sectionEl) {
    const rows = [];
    sectionEl.querySelectorAll('.fb-map-row').forEach((rowEl) => {
      const keyEl = rowEl.querySelector('[data-field="key"]');
      const exprEl = rowEl.querySelector('[data-field="expression"]');
      rows.push({ key: keyEl?.value ?? '', expression: exprEl?.value ?? '' });
    });
    return rows;
  }

  // Re-renders one mapping section's row list in place (after add/remove)
  // without rebuilding the whole tab — preserves the other section's focus.
  _renderMappingSection(sectionEl, mapping, rows) {
    const list = sectionEl.querySelector('[data-role="rows"]');
    if (!list) return;
    list.innerHTML = rows.length
      ? rows.map((r, i) => this._renderMappingRow(mapping, r, i)).join('')
      : `<div class="fb-vars-empty">${escapeHtml(I18n.t(
          mapping === 'input_mapping' ? 'flows_config.map_input_empty' : 'flows_config.map_output_empty',
        ))}</div>`;
    this._populateMappingKeys(sectionEl, mapping);
  }

  _renderPortsTab() {
    const n = this.node;
    const { inputs, outputs } = this._computePorts(n);

    const listHtml = (list, side) => {
      if (list.length === 0) {
        const key = side === 'in' ? 'flows_config.no_inputs' : 'flows_config.no_outputs';
        return `<div class="fb-field-hint">${escapeHtml(I18n.t(key))}</div>`;
      }
      return `<ul class="fb-port-list">${list.map((p) => {
        const t = (p.type || 'any').toLowerCase();
        return `<li><span class="fb-port-dot fb-port-type-${escapeAttr(t)}" aria-hidden="true" title="${escapeAttr(t)}"></span><code>${escapeHtml(p.name)}</code><span class="fb-port-type-tag">${escapeHtml(t)}</span></li>`;
      }).join('')}</ul>`;
    };

    const dynamicHtml = (n.type === 'switch' || n.type === 'router')
      ? this._renderSwitchCasesEditor(n)
      : '';

    return `
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.ports_inputs'))}</label>
        ${listHtml(inputs, 'in')}
      </div>
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.ports_outputs'))}</label>
        ${listHtml(outputs, 'out')}
      </div>
      ${dynamicHtml}
    `;
  }

  _computePorts(n) {
    const isTrigger = n.type === 'trigger' || n.type === 'start';
    const isOutput = n.type === 'output' || n.type === 'end';
    // Adapter metadata z backendu ma priorytet (input_ports/output_ports +
    // input_port_types/output_port_types z `FlowDataType::as_wire_str`).
    const tpl = this.template;
    const tplIn = (tpl && Array.isArray(tpl.input_ports) && tpl.input_ports.length > 0) ? tpl.input_ports : null;
    const tplOut = (tpl && Array.isArray(tpl.output_ports) && tpl.output_ports.length > 0) ? tpl.output_ports : null;
    const tplInTypes = (tpl && Array.isArray(tpl.input_port_types)) ? tpl.input_port_types : null;
    const tplOutTypes = (tpl && Array.isArray(tpl.output_port_types)) ? tpl.output_port_types : null;
    const withType = (names, types) => names.map((name, i) => ({
      name,
      type: (types && typeof types[i] === 'string') ? types[i] : 'any',
    }));
    const inputs = tplIn
      ? withType(tplIn, tplInTypes)
      : (isTrigger ? [] : [{ name: 'in', type: 'any' }]);
    let outputs;
    if (tplOut) {
      outputs = withType(tplOut, tplOutTypes);
    } else if (n.type === 'condition') outputs = [{ name: 'true', type: 'any' }, { name: 'false', type: 'any' }];
    else if (n.type === 'switch' || n.type === 'router') {
      const cases = Array.isArray(n.config?.cases) ? n.config.cases : [];
      if (cases.length > 0) {
        outputs = cases.map((c, i) => ({ name: typeof c === 'string' ? c : (c.name || `case_${i + 1}`), type: 'any' }));
        outputs.push({ name: 'default', type: 'any' });
      } else {
        outputs = [{ name: 'case_1', type: 'any' }, { name: 'case_2', type: 'any' }, { name: 'default', type: 'any' }];
      }
    } else if (isOutput) outputs = [];
    else outputs = [{ name: 'full', type: 'any' }];
    return { inputs, outputs };
  }

  _renderSwitchCasesEditor(n) {
    const cases = Array.isArray(n.config?.cases) ? n.config.cases : ['case_1', 'case_2'];
    const rows = cases.map((c, i) => {
      const name = typeof c === 'string' ? c : (c.name || `case_${i + 1}`);
      return `
        <div class="fb-field-row" data-case-idx="${i}">
          <tf-input data-bind-case="${i}" value="${escapeAttr(name)}"></tf-input>
          <tf-button variant="ghost" size="sm" icon="trash" data-action="remove-case" data-idx="${i}"></tf-button>
        </div>`;
    }).join('');
    return `
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.cases_label'))}</label>
        <div class="fb-field-hint">${I18n.t('flows_config.cases_hint')}</div>
        <div data-role="cases-list" style="display:flex;flex-direction:column;gap:6px;">${rows}</div>
        <tf-button variant="secondary" size="sm" icon="plus" data-action="add-case">${escapeHtml(I18n.t('flows_config.cases_add'))}</tf-button>
      </div>
    `;
  }

  _renderAdvancedTab() {
    const n = this.node;
    return `
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.advanced_node_id'))}</label>
        <tf-input value="${escapeAttr(n.id)}" readonly></tf-input>
      </div>
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.advanced_position'))}</label>
        <div style="display:flex; gap:8px;">
          <tf-input type="number" data-bind-pos="x" value="${n.x}"></tf-input>
          <tf-input type="number" data-bind-pos="y" value="${n.y}"></tf-input>
        </div>
      </div>
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.advanced_raw'))}</label>
        <tf-textarea data-bind-raw="config" rows="6" value="${escapeAttr(JSON.stringify(n.config || {}, null, 2))}"></tf-textarea>
        <div class="fb-field-hint">${escapeHtml(I18n.t('flows_config.advanced_raw_hint'))}</div>
      </div>
    `;
  }

  _bindAdvancedInputs(body) {
    // The node is captured once, as in `_bindConfigInputs`: selecting another
    // node reassigns `this.node` before the panel is re-rendered, and tearing a
    // focused field out fires its `change` on the way — which would write this
    // node's position, or its pasted JSON, onto the node just selected.
    const node = this.node;
    body.querySelectorAll('[data-bind-pos]').forEach((el) => {
      el.addEventListener('change', () => {
        const axis = el.dataset.bindPos;
        const v = parseInt(el.value, 10) || 0;
        this.opts.onPositionChange?.(node.id, { [axis]: v });
      });
    });
    const raw = body.querySelector('[data-bind-raw="config"]');
    if (raw) {
      raw.addEventListener('change', () => {
        try {
          const parsed = JSON.parse(raw.value);
          this.opts.onRawConfigChange?.(node.id, parsed);
        } catch (_) { /* czekamy aż użytkownik naprawi */ }
      });
    }
  }

  /// Re-renders the "Podgląd konfiguracji" block from the node's CURRENT
  /// config. The panel renders that JSON once when it opens, so every later
  /// edit left it showing the values the node had on open — a `bus_publish`
  /// node whose instance and topic were both set still displayed
  /// `"instance_id": "", "topic": ""`, which reads as "nothing was saved".
  /// `onConfigChange` routes through `canvas.updateNodeConfig`, which mutates
  /// this same node object, so re-reading `this.node` here is enough.
  _refreshPreview() {
    const el = this.root?.querySelector('.fb-config-preview');
    const n = this.node;
    if (!el || !n) return;
    el.innerHTML = this._jsonPreview({ label: n.label, type: n.type, config: n.config });
  }

  _jsonPreview(obj) {
    // Prosta kolorowa serializacja JSON
    const json = JSON.stringify(obj, null, 2);
    return escapeHtml(json)
      .replace(/&quot;([^&]+)&quot;(\s*:)/g, '<span class="k">&quot;$1&quot;</span>$2')
      .replace(/:\s*&quot;([^&]*)&quot;/g, ': <span class="s">&quot;$1&quot;</span>')
      .replace(/:\s*(-?\d+\.?\d*)/g, ': <span class="n">$1</span>')
      .replace(/:\s*(true|false|null)/g, ': <span class="n">$1</span>');
  }

  destroy() {
    this.root.innerHTML = '';
  }
}
