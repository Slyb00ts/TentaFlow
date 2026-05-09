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
    if (source === 'prompts') {
      const list = await ApiBinary.list('promptListRequest', { arrayKey: 'prompts' }).catch(() => []);
      return (Array.isArray(list) ? list : []).map((p) => ({
        value: p.id || p.promptId || '',
        label: p.name || p.id || '',
      }));
    }
    return [];
  })();
  _dynamicEnumCache.set(key, promise);
  return promise;
}

const TYPE_ICON = {
  trigger: 'bolt', start: 'bolt',
  llm: 'chip', embeddings: 'sparkle', reranker: 'sparkle',
  stt: 'mic', tts: 'speaker',
  rag: 'rag-db', memory: 'rag-db',
  condition: 'branch', switch: 'branch',
  template: 'code', transform: 'transform', router: 'transform',
  pii_filter: 'shield', tts_clean: 'shield',
  output: 'arrow-out', end: 'arrow-out',
  conversation_history: 'rag-db', session_context: 'rag-db',
  speaker_context: 'rag-db', memory_analyzer: 'sparkle',
};
const TYPE_VAR = {
  trigger: '--node-trigger', start: '--node-start',
  llm: '--node-llm', stt: '--node-stt', tts: '--node-tts',
  rag: '--node-rag', memory: '--node-memory',
  embeddings: '--node-embeddings', reranker: '--node-reranker',
  condition: '--node-condition', switch: '--node-switch',
  template: '--node-template', transform: '--node-transform',
  pii_filter: '--node-pii_filter', tts_clean: '--node-tts_clean',
  router: '--node-router', output: '--node-output', end: '--node-end',
};

export class FlowConfig {
  constructor(rootEl, opts = {}) {
    this.root = rootEl;
    this.opts = opts;
    this.node = null;
    this.template = null;
    this.activeTab = 'config';
    this.root.classList.add('fb-config');
    this.renderEmpty();
  }

  setTemplate(tpl) { this.template = tpl; }

  show(node, template) {
    this.node = node;
    this.template = template;
    this.activeTab = 'config';
    if (!node) { this.renderEmpty(); return; }
    this._render();
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
    const iconId = TYPE_ICON[n.type] || 'chip';
    const varName = TYPE_VAR[n.type] || '--node-llm';
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
      <nav class="fb-config-tabs" role="tablist">
        <button class="fb-config-tab ${this.activeTab === 'config' ? 'active' : ''}" data-tab="config">${escapeHtml(I18n.t('flows_config.tab_config'))}</button>
        <button class="fb-config-tab ${this.activeTab === 'ports' ? 'active' : ''}" data-tab="ports">${escapeHtml(I18n.t('flows_config.tab_ports'))}</button>
        <button class="fb-config-tab ${this.activeTab === 'advanced' ? 'active' : ''}" data-tab="advanced">${escapeHtml(I18n.t('flows_config.tab_advanced'))}</button>
      </nav>
      <div class="fb-config-body" data-role="body"></div>
      <footer class="fb-config-footer">
        <tf-button variant="secondary" size="sm" icon="copy" data-action="duplicate">${escapeHtml(I18n.t('flows_config.duplicate'))}</tf-button>
        <tf-button variant="danger" size="sm" icon="trash" data-action="delete">${escapeHtml(I18n.t('flows_config.delete'))}</tf-button>
      </footer>
    `;

    this.root.querySelectorAll('.fb-config-tab').forEach((t) => {
      t.addEventListener('click', () => {
        this.activeTab = t.dataset.tab;
        this._renderBody();
        this.root.querySelectorAll('.fb-config-tab').forEach((x) => x.classList.toggle('active', x.dataset.tab === this.activeTab));
      });
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
    else if (this.activeTab === 'ports') body.innerHTML = this._renderPortsTab();
    else body.innerHTML = this._renderAdvancedTab();

    if (this.activeTab === 'config') this._bindConfigInputs(body);
    if (this.activeTab === 'advanced') this._bindAdvancedInputs(body);
    if (this.activeTab === 'ports') this._bindPortsInputs(body);
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
      });
    });
    const addBtn = body.querySelector('[data-action="add-case"]');
    addBtn?.addEventListener('click', () => {
      const current = readCases();
      current.push(`case_${current.length + 1}`);
      this.opts.onConfigChange?.(n.id, { cases: current });
      // Re-render ports tab żeby pojawił się nowy wiersz
      this._renderBody();
    });
    body.querySelectorAll('[data-action="remove-case"]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const idx = parseInt(btn.dataset.idx, 10);
        const current = readCases();
        current.splice(idx, 1);
        this.opts.onConfigChange?.(n.id, { cases: current });
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

    let html = `
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.name'))}</label>
        <input class="fb-input" data-bind="label" value="${escapeAttr(isAutoNodeLabel(n.label, n.type, this.template?.label) ? '' : (n.label || ''))}" placeholder="${escapeAttr(getNodeName(n.type, this.template?.label))}">
      </div>
    `;

    for (const key of keys) {
      const def = props[key];
      if (!def) continue;
      const value = n.config?.[key];
      html += this._renderField(key, def, value, required.includes(key));
    }

    if (keys.length === 0) {
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
    const reqMark = isRequired ? ' <span style="color:var(--tf-danger);">*</span>' : '';

    if (type === 'boolean') {
      return `
        <div class="fb-field fb-field-row">
          <div>
            <label class="fb-label">${escapeHtml(title)}${reqMark}</label>
            ${hint ? `<div class="fb-field-hint">${escapeHtml(hint)}</div>` : ''}
          </div>
          <span class="fb-toggle ${curVal ? 'on' : ''}" role="switch" data-bind="${escapeAttr(key)}" data-type="boolean"></span>
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
          <label class="fb-label">${escapeHtml(title)}${reqMark}</label>
          <select class="fb-select" data-bind="${escapeAttr(key)}" data-type="string">${opts}</select>
          ${hint ? `<div class="fb-field-hint">${escapeHtml(hint)}</div>` : ''}
        </div>`;
    }

    if (def.dynamic_enum && typeof def.dynamic_enum === 'object') {
      // Renderujemy placeholder select; opcje zaciagamy async po renderze
      // (loadDynamicEnumOptions z cache). Aktualna wartosc trzymana jako
      // jedyna opcja zeby preview JSON pokazywal poprawnie.
      const source = String(def.dynamic_enum.source || '');
      const category = String(def.dynamic_enum.category || '');
      const placeholder = curVal ? escapeHtml(String(curVal)) : '— wybierz —';
      return `
        <div class="fb-field">
          <label class="fb-label">${escapeHtml(title)}${reqMark}</label>
          <select class="fb-select" data-bind="${escapeAttr(key)}" data-type="string"
                  data-dynamic-source="${escapeAttr(source)}"
                  data-dynamic-category="${escapeAttr(category)}">
            <option value="${escapeAttr(curVal || '')}" selected>${placeholder}</option>
          </select>
          ${hint ? `<div class="fb-field-hint">${escapeHtml(hint)}</div>` : ''}
        </div>`;
    }

    if (type === 'number' || type === 'integer') {
      const hasRange = def.minimum != null && def.maximum != null;
      if (hasRange) {
        return `
          <div class="fb-field">
            <div class="fb-field-row">
              <label class="fb-label">${escapeHtml(title)}${reqMark}</label>
              <span class="fb-range-val" data-role="rangeval-${escapeAttr(key)}">${escapeHtml(String(curVal))}</span>
            </div>
            <input class="fb-range" type="range" min="${def.minimum}" max="${def.maximum}" step="${def.step || (type === 'integer' ? 1 : 0.1)}" value="${escapeAttr(String(curVal))}" data-bind="${escapeAttr(key)}" data-type="number">
            ${hint ? `<div class="fb-field-hint">${escapeHtml(hint)}</div>` : ''}
          </div>`;
      }
      return `
        <div class="fb-field">
          <label class="fb-label">${escapeHtml(title)}${reqMark}</label>
          <input class="fb-input" type="number" step="${def.step || (type === 'integer' ? 1 : 'any')}" value="${escapeAttr(String(curVal))}" data-bind="${escapeAttr(key)}" data-type="number">
          ${hint ? `<div class="fb-field-hint">${escapeHtml(hint)}</div>` : ''}
        </div>`;
    }

    if (def.format === 'textarea' || (typeof curVal === 'string' && curVal.length > 80)) {
      return `
        <div class="fb-field">
          <label class="fb-label">${escapeHtml(title)}${reqMark}</label>
          <textarea class="fb-textarea" data-bind="${escapeAttr(key)}" data-type="string" placeholder="${escapeAttr(def.placeholder || '')}">${escapeHtml(String(curVal))}</textarea>
          ${hint ? `<div class="fb-field-hint">${escapeHtml(hint)}</div>` : ''}
        </div>`;
    }

    return `
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(title)}${reqMark}</label>
        <input class="fb-input" type="text" value="${escapeAttr(String(curVal))}" data-bind="${escapeAttr(key)}" data-type="string" placeholder="${escapeAttr(def.placeholder || '')}">
        ${hint ? `<div class="fb-field-hint">${escapeHtml(hint)}</div>` : ''}
      </div>`;
  }

  _bindConfigInputs(body) {
    body.querySelectorAll('[data-bind]').forEach((el) => {
      const key = el.dataset.bind;
      if (key === 'label') {
        el.addEventListener('change', () => {
          this.opts.onLabelChange?.(this.node.id, el.value);
        });
        return;
      }
      if (el.classList.contains('fb-toggle')) {
        el.addEventListener('click', () => {
          const on = !el.classList.contains('on');
          el.classList.toggle('on', on);
          this.opts.onConfigChange?.(this.node.id, { [key]: on });
        });
        return;
      }
      const type = el.dataset.type;
      const ev = el.type === 'range' ? 'input' : 'change';
      el.addEventListener(ev, () => {
        let v = el.value;
        if (type === 'number') v = parseFloat(v);
        const rv = body.querySelector(`[data-role="rangeval-${CSS.escape(key)}"]`);
        if (rv) rv.textContent = String(v);
        this.opts.onConfigChange?.(this.node.id, { [key]: v });
      });
    });

    // Async populate dynamic_enum dropdownow. Bierzemy aktualna wartosc z
    // node.config zeby zachowac selekcje po refresh listy. Jak fetch
    // failuje, zostawiamy single-option placeholder + log do konsoli.
    body.querySelectorAll('select[data-dynamic-source]').forEach(async (sel) => {
      const source = sel.dataset.dynamicSource;
      const category = sel.dataset.dynamicCategory || '';
      const key = sel.dataset.bind;
      const currentValue = (this.node.config && this.node.config[key]) || '';
      try {
        const opts = await loadDynamicEnumOptions(source, category);
        if (!opts.length) {
          sel.innerHTML = `<option value="" disabled selected>— brak dostepnych ${source} ${category} —</option>`;
          return;
        }
        const html = [
          `<option value="" ${currentValue ? '' : 'selected'}>— wybierz —</option>`,
          ...opts.map((o) => {
            const sel2 = String(currentValue) === String(o.value) ? 'selected' : '';
            return `<option value="${escapeAttr(o.value)}" ${sel2}>${escapeHtml(o.label)}</option>`;
          }),
        ].join('');
        sel.innerHTML = html;
        // Jesli aktualna wartosc nie jest na liscie (np. usuniety alias), zostawiamy
        // placeholder selected — user widzi ze cos przepadlo i moze wybrac inny.
        if (currentValue && !opts.some((o) => String(o.value) === String(currentValue))) {
          const stale = document.createElement('option');
          stale.value = currentValue;
          stale.textContent = `${currentValue} (niedostepne)`;
          stale.selected = true;
          sel.appendChild(stale);
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(`[fb-config] dynamic_enum load failed for ${source}:${category}:`, err);
      }
    });
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
          <input class="fb-input" data-bind-case="${i}" value="${escapeAttr(name)}">
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
        <input class="fb-input" value="${escapeAttr(n.id)}" readonly>
      </div>
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.advanced_position'))}</label>
        <div style="display:flex; gap:8px;">
          <input class="fb-input" type="number" data-bind-pos="x" value="${n.x}">
          <input class="fb-input" type="number" data-bind-pos="y" value="${n.y}">
        </div>
      </div>
      <div class="fb-field">
        <label class="fb-label">${escapeHtml(I18n.t('flows_config.advanced_raw'))}</label>
        <textarea class="fb-textarea" data-bind-raw="config" rows="6">${escapeHtml(JSON.stringify(n.config || {}, null, 2))}</textarea>
        <div class="fb-field-hint">${escapeHtml(I18n.t('flows_config.advanced_raw_hint'))}</div>
      </div>
    `;
  }

  _bindAdvancedInputs(body) {
    body.querySelectorAll('[data-bind-pos]').forEach((el) => {
      el.addEventListener('change', () => {
        const axis = el.dataset.bindPos;
        const v = parseInt(el.value, 10) || 0;
        this.opts.onPositionChange?.(this.node.id, { [axis]: v });
      });
    });
    const raw = body.querySelector('[data-bind-raw="config"]');
    if (raw) {
      raw.addEventListener('change', () => {
        try {
          const parsed = JSON.parse(raw.value);
          this.opts.onRawConfigChange?.(this.node.id, parsed);
        } catch (_) { /* czekamy aż użytkownik naprawi */ }
      });
    }
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
