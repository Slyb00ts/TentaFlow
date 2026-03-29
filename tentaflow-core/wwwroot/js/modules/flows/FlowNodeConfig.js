// =============================================================================
// Plik: modules/flows/FlowNodeConfig.js
// Opis: Panel konfiguracji wybranego wezla flow - formularz dynamiczny
//       generowany na podstawie typu wezla.
// Przyklad: FlowNodeConfig.init(panelEl, onUpdate);
//           FlowNodeConfig.showNode(node);
// =============================================================================

const FlowNodeConfig = (() => {
  'use strict';

  let panelEl = null;
  let currentNode = null;
  let onUpdateCallback = null;
  let cachedPrompts = null;
  let cachedServices = null;

  // Definicje pol konfiguracji wg typu wezla
  const NODE_FIELDS = {
    trigger: [],
    llm: [
      { key: 'model', label: 'flows.fields.model', type: 'async_select', source: 'services', filter: 'llm' },
      { key: 'prompt_id', label: 'flows.fields.prompt', type: 'async_select', source: 'prompts' },
      { key: 'system_prompt', label: 'flows.fields.system_prompt', type: 'textarea', placeholder: 'Fallback...' },
      { key: 'temperature', label: 'flows.fields.temperature', type: 'range', min: 0, max: 2, step: 0.1, default: 0.7 },
      { key: 'max_tokens', label: 'flows.fields.max_tokens', type: 'number', default: 4096 },
      { key: 'stream', label: 'flows.fields.streaming', type: 'checkbox', default: true },
      { key: 'use_messages_context', label: 'flows.fields.use_context', type: 'checkbox', default: false },
    ],
    stt: [
      { key: 'language', label: 'flows.fields.language', type: 'select', options: [
        { value: 'pl', label: 'Polski' },
        { value: 'en', label: 'English' },
        { value: 'de', label: 'Deutsch' },
      ], default: 'pl' },
      { key: 'model', label: 'flows.fields.model', type: 'async_select', source: 'services', filter: 'stt' },
    ],
    tts: [
      { key: 'language', label: 'flows.fields.language', type: 'select', options: [
        { value: 'pl', label: 'Polski' },
        { value: 'en', label: 'English' },
      ], default: 'pl' },
      { key: 'voice', label: 'flows.fields.voice', type: 'text', placeholder: 'e.g. nova' },
      { key: 'speed', label: 'flows.fields.speed', type: 'range', min: 0.5, max: 2, step: 0.1, default: 1.0 },
    ],
    rag: [
      { key: 'service', label: 'flows.fields.service', type: 'async_select', source: 'services', filter: 'rag' },
      { key: 'collection', label: 'flows.fields.collection', type: 'text', placeholder: 'e.g. knowledge_base' },
      { key: 'top_k', label: 'flows.fields.top_k', type: 'number', default: 5 },
      { key: 'min_score', label: 'flows.fields.min_score', type: 'range', min: 0, max: 1, step: 0.05, default: 0.7 },
    ],
    memory: [
      { key: 'mode', label: 'flows.fields.mode', type: 'select', options: [
        { value: 'query', label: 'Read' },
        { value: 'store', label: 'Write' },
      ], default: 'query' },
      { key: 'memory_type', label: 'flows.fields.memory_type', type: 'select', options: [
        { value: 'conversation', label: 'Conversation' },
        { value: 'long_term', label: 'Long term' },
        { value: 'episodic', label: 'Episodic' },
      ], default: 'conversation' },
      { key: 'max_entries', label: 'flows.fields.max_entries', type: 'number', default: 10 },
      { key: 'inject_to_messages', label: 'flows.fields.inject_to_messages', type: 'checkbox', default: false },
      { key: 'context_prompt_id', label: 'flows.fields.context_prompt', type: 'async_select', source: 'prompts' },
    ],
    embeddings: [
      { key: 'model', label: 'flows.fields.model', type: 'async_select', source: 'services', filter: 'embeddings' },
    ],
    condition: [
      { key: 'field', label: 'flows.fields.field', type: 'text', placeholder: 'e.g. should_query' },
      { key: 'operator', label: 'flows.fields.operator', type: 'select', options: [
        { value: 'equals', label: 'Equals (==)' },
        { value: 'not_equals', label: 'Not equals (!=)' },
        { value: 'contains', label: 'Contains' },
        { value: 'not_contains', label: 'Not contains' },
        { value: 'gt', label: 'Greater than (>)' },
        { value: 'gte', label: 'Greater or equal (>=)' },
        { value: 'lt', label: 'Less than (<)' },
        { value: 'lte', label: 'Less or equal (<=)' },
        { value: 'exists', label: 'Exists' },
        { value: 'not_exists', label: 'Not exists' },
        { value: 'is_empty', label: 'Empty' },
        { value: 'is_not_empty', label: 'Not empty' },
      ], default: 'equals' },
      { key: 'value', label: 'flows.fields.value', type: 'text', placeholder: 'Expected value...' },
      { key: 'true_label', label: 'flows.fields.true_label', type: 'text', default: 'yes' },
      { key: 'false_label', label: 'flows.fields.false_label', type: 'text', default: 'no' },
    ],
    switch: [
      { key: 'field', label: 'flows.fields.field', type: 'text', placeholder: 'e.g. intent' },
      { key: 'cases', label: 'flows.fields.cases', type: 'case_list' },
    ],
    template: [
      { key: 'template', label: 'flows.fields.template', type: 'textarea', placeholder: 'Use {input}, {model}, {var}...' },
    ],
    pii_filter: [],
    tts_clean: [],
    conversation_history: [
      { key: 'max_messages', label: 'flows.fields.max_messages', type: 'number', default: 20 },
    ],
    session_context: [
      { key: 'first_prompt_id', label: 'flows.fields.prompt_start', type: 'async_select', source: 'prompts' },
      { key: 'continue_prompt_id', label: 'flows.fields.prompt_continue', type: 'async_select', source: 'prompts' },
      { key: 'unclear_prompt_id', label: 'flows.fields.prompt_unclear', type: 'async_select', source: 'prompts' },
    ],
    speaker_context: [
      { key: 'high_threshold', label: 'flows.fields.high_threshold', type: 'range', min: 0, max: 1, step: 0.05, default: 0.85 },
      { key: 'medium_threshold', label: 'flows.fields.medium_threshold', type: 'range', min: 0, max: 1, step: 0.05, default: 0.60 },
      { key: 'personalization_first_prompt', label: 'flows.fields.personalization_start', type: 'async_select', source: 'prompts' },
      { key: 'personalization_continue_prompt', label: 'flows.fields.personalization_next', type: 'async_select', source: 'prompts' },
      { key: 'unknown_user_prompt', label: 'flows.fields.unknown_user', type: 'async_select', source: 'prompts' },
      { key: 'medium_confidence_known_prompt', label: 'flows.fields.med_conf_known', type: 'async_select', source: 'prompts' },
      { key: 'medium_confidence_unknown_prompt', label: 'flows.fields.med_conf_unknown', type: 'async_select', source: 'prompts' },
      { key: 'new_voice_prompt', label: 'flows.fields.new_voice', type: 'async_select', source: 'prompts' },
      { key: 'new_speaker_prompt', label: 'flows.fields.new_speaker', type: 'async_select', source: 'prompts' },
    ],
    memory_analyzer: [
      { key: 'mode', label: 'flows.fields.mode', type: 'select', options: [
        { value: 'query_analysis', label: 'Query analysis' },
        { value: 'store_analysis', label: 'Store analysis' },
      ], default: 'query_analysis' },
      { key: 'prompt_id', label: 'flows.fields.prompt', type: 'async_select', source: 'prompts' },
    ],
    router: [],
    output: [
      { key: 'format', label: 'flows.fields.format', type: 'select', options: [
        { value: 'text', label: 'Text' },
        { value: 'json', label: 'JSON' },
        { value: 'stream', label: 'Stream' },
      ], default: 'text' },
    ],
  };

  // Schemat wejsc/wyjsc wezlow - PRAWDZIWE pola z node_results
  const NODE_IO_SCHEMA = {
    trigger: {
      inputs: [],
      outputs: [
        { name: 'input', format: 'text', desc: 'User message text' },
        { name: 'model', format: 'text', desc: 'Model name' },
        { name: 'request_id', format: 'text', desc: 'Request ID' },
      ],
      shared: 'Sets ctx.input and ctx.messages',
    },
    conversation_history: {
      inputs: [
        { name: 'ctx.input', format: 'text', desc: 'User message (shared state)' },
        { name: 'ctx.messages', format: 'json', desc: 'Message list (shared state)' },
      ],
      outputs: [
        { name: 'is_first_message', format: 'bool', desc: 'Is first message' },
        { name: 'history_count', format: 'number', desc: 'Messages count in history' },
        { name: 'session_id', format: 'text', desc: 'Session ID' },
      ],
      shared: 'Injects history into ctx.messages',
    },
    session_context: {
      inputs: [
        { name: 'ctx.messages', format: 'json', desc: 'Messages (shared state)' },
      ],
      outputs: [
        { name: 'session_type', format: 'text', desc: '"first" | "continue" | "unclear"' },
        { name: 'prompt_id', format: 'text', desc: 'Used prompt (session_start/continue/unclear)' },
        { name: 'is_first_message', format: 'bool', desc: 'Is first message' },
      ],
      shared: 'Adds session suffix to ctx.messages',
    },
    pii_filter: {
      inputs: [
        { name: 'text', format: 'text', desc: 'From resolve_input_text' },
      ],
      outputs: [
        { name: 'text', format: 'text', desc: 'Text after PII filtering' },
        { name: 'rules_applied', format: 'number', desc: 'Applied rules count' },
      ],
      shared: 'Mutates ctx.input and last user message in ctx.messages',
    },
    speaker_context: {
      inputs: [
        { name: 'ctx.person_id', format: 'text', desc: 'Speaker ID (shared state)' },
        { name: 'ctx.speaker_confidence', format: 'number', desc: 'Confidence (shared state)' },
      ],
      outputs: [
        { name: 'recognized', format: 'bool', desc: 'Is speaker recognized' },
        { name: 'person_name', format: 'text', desc: 'Person name' },
        { name: 'confidence_level', format: 'text', desc: '"high" | "medium" | "low" | "none"' },
        { name: 'confidence', format: 'number', desc: 'Confidence value 0-1' },
      ],
      shared: 'Adds personalization to ctx.messages',
    },
    memory_analyzer: {
      inputs: [
        { name: 'ctx.input', format: 'text', desc: 'User message (shared state)' },
      ],
      outputs: [
        { name: 'text', format: 'text', desc: 'Input text (propagated)' },
        { name: 'should_query', format: 'bool', desc: 'Should query memory' },
        { name: 'query_type', format: 'text', desc: '"NewSearch" | "Refine" | "None"' },
        { name: 'search_terms', format: 'json', desc: 'Search terms list' },
      ],
    },
    condition: {
      inputs: [
        { name: 'field', format: 'any', desc: 'Field from predecessor output' },
      ],
      outputs: [
        { name: 'result', format: 'bool', desc: 'Condition result (true/false)' },
        { name: 'field', format: 'text', desc: 'Checked field name' },
        { name: 'operator', format: 'text', desc: 'Used operator' },
      ],
    },
    memory: {
      inputs: [
        { name: 'query', format: 'text', desc: 'From memory_analyzer or resolve_input_text' },
      ],
      outputs: [
        { name: 'text', format: 'text', desc: 'Memory context' },
        { name: 'memories', format: 'json', desc: 'Found memories [{id, label, score}]' },
        { name: 'relevance', format: 'number', desc: 'Avg relevance' },
        { name: 'answers_count', format: 'number', desc: 'Answers count' },
      ],
      shared: 'If inject_to_messages=true, injects context to ctx.messages',
    },
    llm: {
      inputs: [
        { name: 'ctx.messages', format: 'json', desc: 'Messages (if use_messages_context=true)' },
        { name: 'ctx.input', format: 'text', desc: 'Text (if use_messages_context=false)' },
      ],
      outputs: [
        { name: 'text', format: 'text', desc: 'Model response' },
        { name: 'content', format: 'text', desc: 'Response (alias)' },
        { name: 'tokens', format: 'json', desc: '{prompt, completion}' },
        { name: 'model', format: 'text', desc: 'Used model name' },
      ],
    },
    tts_clean: {
      inputs: [
        { name: 'text', format: 'text', desc: 'From resolve_input_text' },
      ],
      outputs: [
        { name: 'text', format: 'text', desc: 'Text after cleaning' },
        { name: 'rules_applied', format: 'number', desc: 'Applied rules count' },
      ],
    },
    rag: {
      inputs: [
        { name: 'text', format: 'text', desc: 'From resolve_input_text' },
      ],
      outputs: [
        { name: 'text', format: 'text', desc: 'Knowledge base context' },
        { name: 'context', format: 'text', desc: 'Context (alias)' },
        { name: 'sources', format: 'json', desc: 'Document sources' },
        { name: 'score', format: 'number', desc: 'Avg score' },
        { name: 'chunks_count', format: 'number', desc: 'Chunks count' },
      ],
    },
    stt: {
      inputs: [
        { name: 'ctx.audio_input', format: 'binary', desc: 'Audio data (shared state)' },
      ],
      outputs: [
        { name: 'text', format: 'text', desc: 'Recognized text' },
        { name: 'language', format: 'text', desc: 'Detected language' },
        { name: 'duration', format: 'number', desc: 'Duration' },
      ],
    },
    tts: {
      inputs: [
        { name: 'text', format: 'text', desc: 'From resolve_input_text' },
      ],
      outputs: [
        { name: 'audio_base64', format: 'text', desc: 'Audio base64' },
        { name: 'format', format: 'text', desc: 'Format (wav/mp3)' },
        { name: 'duration', format: 'number', desc: 'Duration' },
      ],
    },
    embeddings: {
      inputs: [
        { name: 'text', format: 'text', desc: 'From resolve_input_text' },
      ],
      outputs: [
        { name: 'embedding', format: 'json', desc: 'Embedding vector' },
        { name: 'dimensions', format: 'number', desc: 'Vector dimensions' },
      ],
    },
    template: {
      inputs: [
        { name: 'ctx.input', format: 'text', desc: 'Available as {input}' },
        { name: 'ctx.variables', format: 'json', desc: 'Variables as {var}' },
      ],
      outputs: [
        { name: 'text', format: 'text', desc: 'Text after replacement' },
      ],
    },
    output: {
      inputs: [
        { name: 'text', format: 'text', desc: 'From resolve_input_text' },
      ],
      outputs: [
        { name: 'text', format: 'text', desc: 'Flow output' },
      ],
    },
  };

  // Opisy typow wezlow - zdefiniowane w FlowCanvas.NODE_DESCRIPTIONS
  const NODE_DESCRIPTIONS = FlowCanvas.NODE_DESCRIPTIONS;

  // Inicjalizacja panelu
  function init(container, onUpdate) {
    panelEl = container;
    onUpdateCallback = onUpdate;
    showEmpty();
  }

  // Pokaz pusty panel
  function showEmpty() {
    if (!panelEl) return;
    panelEl.innerHTML = `
      <div class="flow-config-header">
        <span class="flow-config-title" data-i18n="nav.settings">${I18n.t('nav.settings')}</span>
      </div>
      <div style="color: var(--color-text-muted); font-size: var(--font-size-sm); text-align: center; padding-top: 40px;" data-i18n="flows.select_node_hint">
        ${I18n.t('flows.select_node_hint')}
      </div>
    `;
  }

  // Pokaz konfiguracje wezla
  async function showNode(node) {
    if (!panelEl || !node) {
      showEmpty();
      return;
    }
    currentNode = node;

    const fields = NODE_FIELDS[node.type] || [];
    const config = node.config || {};
    const desc = I18n.t(`flows.node_descriptions.${node.type}`) || '';
    const localizedNodeName = I18n.t(`flows.node_names.${node.type}`) || node.type;

    let html = `
      <div class="flow-config-header">
        <span class="flow-config-title">${Utils.escapeHtml(node.label || localizedNodeName)}</span>
        <button class="flow-config-close" id="fc-close">&times;</button>
      </div>
      ${desc ? `<div class="flow-config-description">${Utils.escapeHtml(desc)}</div>` : ''}

      <div class="flow-config-section">
        <div class="flow-config-section-header">Basic</div>
        <div class="form-group">
          <label for="fc-label" data-i18n="common.name">${I18n.t('common.name')}</label>
          <input type="text" id="fc-label" value="${Utils.escapeAttr(node.label || '')}" placeholder="Node name">
        </div>
      </div>
    `;

    if (fields.length > 0) {
      html += `<div class="flow-config-section"><div class="flow-config-section-header" data-i18n="playground.params">${I18n.t('playground.params')}</div>`;

      for (const field of fields) {
        const value = config[field.key] != null ? config[field.key] : (field.default != null ? field.default : '');
        html += renderField(field, value);
      }

      html += `</div>`;
    }

    // Sekcja I/O
    const ioSchema = NODE_IO_SCHEMA[node.type];
    if (ioSchema) {
      html += `<div class="flow-config-section"><div class="flow-config-section-header" data-i18n="flows.node_io">${I18n.t('flows.node_io')}</div>`;
      if (ioSchema.inputs.length > 0) {
        html += `<div class="flow-io-group"><div class="flow-io-label">IN</div>`;
        for (const inp of ioSchema.inputs) {
          html += `<div class="flow-io-item"><span class="flow-io-badge flow-io-${inp.format}">${inp.format}</span> <span class="flow-io-name">${inp.name}</span><div class="flow-io-desc">${inp.desc}</div></div>`;
        }
        html += `</div>`;
      }
      if (ioSchema.outputs.length > 0) {
        html += `<div class="flow-io-group"><div class="flow-io-label">OUT</div>`;
        for (const out of ioSchema.outputs) {
          html += `<div class="flow-io-item"><span class="flow-io-badge flow-io-${out.format}">${out.format}</span> <span class="flow-io-name">${out.name}</span><div class="flow-io-desc">${out.desc}</div></div>`;
        }
        html += `</div>`;
      }
      if (ioSchema.shared) {
        html += `<div class="flow-io-shared">${ioSchema.shared}</div>`;
      }
      html += `</div>`;
    }

    // Przycisk usuwania
    html += `
      <div class="flow-config-section" style="margin-top: auto; padding-top: 16px; border-top: 1px solid var(--color-border);">
        <button class="btn btn-ghost btn-sm btn-block" id="fc-delete-node" style="color: var(--color-error);" data-i18n="flows.delete_node">${I18n.t('flows.delete_node')}</button>
      </div>
    `;

    panelEl.innerHTML = html;

    // Podepnij zdarzenia
    panelEl.querySelector('#fc-close')?.addEventListener('click', () => {
      showEmpty();
      if (onUpdateCallback) onUpdateCallback(null, 'deselect');
    });

    panelEl.querySelector('#fc-label')?.addEventListener('change', (e) => {
      if (currentNode) {
        currentNode.label = e.target.value;
        notifyUpdate();
      }
    });

    panelEl.querySelector('#fc-delete-node')?.addEventListener('click', () => {
      if (currentNode && onUpdateCallback) {
        onUpdateCallback(currentNode.id, 'delete');
      }
    });

    // Zdarzenia pol konfiguracji
    for (const field of fields) {
      if (field.type === 'case_list') continue;
      const el = panelEl.querySelector(`[data-config-key="${field.key}"]`);
      if (!el) continue;

      const eventType = (field.type === 'checkbox') ? 'change' :
                        (field.type === 'range') ? 'input' : 'change';

      el.addEventListener(eventType, () => {
        if (!currentNode) return;
        if (!currentNode.config) currentNode.config = {};

        if (field.type === 'checkbox') {
          currentNode.config[field.key] = el.checked;
        } else if (field.type === 'number' || field.type === 'range') {
          currentNode.config[field.key] = parseFloat(el.value);
          // Aktualizuj wyswietlana wartosc slidera
          const rangeVal = panelEl.querySelector(`#fc-range-val-${field.key}`);
          if (rangeVal) rangeVal.textContent = el.value;
        } else {
          currentNode.config[field.key] = el.value;
        }

        notifyUpdate();
      });
    }

    // Podepnij zdarzenia listy case'ow (Switch)
    bindCaseListEvents();

    // Wypelnij asynchroniczne selecty
    await populateAsyncSelects();
  }

  // Podpiecie zdarzen listy case'ow (Switch)
  function bindCaseListEvents() {
    if (!panelEl || !currentNode) return;

    // Przycisk "Dodaj przypadek"
    const addBtn = panelEl.querySelector('#fc-add-case-cases');
    if (addBtn) {
      addBtn.addEventListener('click', () => {
        if (!currentNode.config) currentNode.config = {};
        if (!Array.isArray(currentNode.config.cases)) currentNode.config.cases = [];
        currentNode.config.cases.push('');
        showNode(currentNode);
      });
    }

    // Przyciski "Usun" przy case'ach
    panelEl.querySelectorAll('[data-remove-case]').forEach(btn => {
      btn.addEventListener('click', () => {
        const idx = parseInt(btn.dataset.removeCase, 10);
        if (currentNode.config && Array.isArray(currentNode.config.cases)) {
          currentNode.config.cases.splice(idx, 1);
          showNode(currentNode);
        }
      });
    });

    // Inputy case'ow - aktualizacja wartosci
    panelEl.querySelectorAll('[data-case-index]').forEach(input => {
      input.addEventListener('change', () => {
        const idx = parseInt(input.dataset.caseIndex, 10);
        if (currentNode.config && Array.isArray(currentNode.config.cases)) {
          currentNode.config.cases[idx] = input.value;
          notifyUpdate();
        }
      });
    });
  }

  // Wypelnianie selectow ladowanych z API
  async function populateAsyncSelects() {
    if (!panelEl) return;
    const selects = panelEl.querySelectorAll('select[data-source]');
    if (selects.length === 0) return;

    for (const sel of selects) {
      const source = sel.dataset.source;
      const filter = sel.dataset.filter || '';
      const configKey = sel.dataset.configKey;
      const currentValue = (currentNode && currentNode.config) ? currentNode.config[configKey] || '' : '';

      let options = [];

      if (source === 'prompts') {
        if (!cachedPrompts) {
          try {
            cachedPrompts = await ApiClient.get('/api/prompts');
          } catch (_) {
            cachedPrompts = [];
          }
        }
        options = (cachedPrompts || []).map(p => ({
          value: p.prompt_id,
          label: p.name,
        }));
      } else if (source === 'services') {
        if (!cachedServices) {
          try {
            cachedServices = await ApiClient.get('/api/services');
          } catch (_) {
            cachedServices = [];
          }
        }
        let items = cachedServices || [];
        if (filter) {
          items = items.filter(s => s.service_type === filter);
        }
        options = items.map(s => ({
          value: s.name,
          label: `${s.name} (${s.status})`,
        }));
      }

      // Zbuduj HTML opcji
      let optHtml = `<option value="">-- --</option>`;
      for (const opt of options) {
        const selected = String(currentValue) === String(opt.value) ? 'selected' : '';
        optHtml += `<option value="${Utils.escapeAttr(opt.value)}" ${selected}>${Utils.escapeHtml(opt.label)}</option>`;
      }
      sel.innerHTML = optHtml;
    }
  }

  // Renderowanie pola formularza
  function renderField(field, value) {
    let html = `<div class="form-group">`;
    const localizedLabel = I18n.t(field.label) || field.label;
    html += `<label>${Utils.escapeHtml(localizedLabel)}</label>`;

    switch (field.type) {
      case 'text':
        html += `<input type="text" data-config-key="${field.key}" value="${Utils.escapeAttr(String(value))}" placeholder="${Utils.escapeAttr(field.placeholder || '')}">`;
        break;

      case 'number':
        html += `<input type="number" data-config-key="${field.key}" value="${value}" min="${field.min || 0}" max="${field.max || 100000}" step="${field.step || 1}">`;
        break;

      case 'range':
        html += `<input type="range" data-config-key="${field.key}" value="${value}" min="${field.min || 0}" max="${field.max || 1}" step="${field.step || 0.1}">`;
        html += `<div class="flow-config-range-value" id="fc-range-val-${field.key}">${value}</div>`;
        break;

      case 'select':
        html += `<select data-config-key="${field.key}">`;
        for (const opt of (field.options || [])) {
          const sel = String(value) === String(opt.value) ? 'selected' : '';
          html += `<option value="${Utils.escapeAttr(opt.value)}" ${sel}>${Utils.escapeHtml(opt.label)}</option>`;
        }
        html += `</select>`;
        break;

      case 'checkbox':
        html += `<label style="display: flex; align-items: center; gap: 8px; cursor: pointer;">`;
        html += `<input type="checkbox" data-config-key="${field.key}" ${value ? 'checked' : ''} style="width: auto;">`;
        html += `<span data-i18n="common.active">${I18n.t('common.active')}</span></label>`;
        break;

      case 'textarea':
        html += `<textarea data-config-key="${field.key}" placeholder="${Utils.escapeAttr(field.placeholder || '')}" style="min-height: 80px;">${Utils.escapeHtml(String(value))}</textarea>`;
        break;

      case 'case_list':
        html += renderCaseList(field.key, value);
        break;

      case 'async_select':
        html += `<select data-config-key="${field.key}" data-source="${field.source}" data-filter="${field.filter || ''}">`;
        html += `<option value="" data-i18n="common.loading">${I18n.t('common.loading')}</option>`;
        html += `</select>`;
        break;
    }

    html += `</div>`;
    return html;
  }

  // Renderowanie listy case'ow (Switch)
  function renderCaseList(key, cases) {
    const items = Array.isArray(cases) ? cases : [];
    let html = `<div class="flow-config-case-list" id="fc-cases-${key}">`;

    for (let i = 0; i < items.length; i++) {
      html += `
        <div class="flow-config-case-item">
          <input type="text" data-case-index="${i}" value="${Utils.escapeAttr(items[i] || '')}" placeholder="${I18n.t('flows.case_value')}">
          <button class="flow-config-case-remove" data-remove-case="${i}">&times;</button>
        </div>
      `;
    }

    html += `</div>`;
    html += `<button class="btn btn-ghost btn-sm" id="fc-add-case-${key}" style="margin-top: 4px;" data-i18n="flows.add_case">+ ${I18n.t('flows.add_case')}</button>`;
    return html;
  }

  // Powiadomienie o aktualizacji
  function notifyUpdate() {
    if (onUpdateCallback && currentNode) {
      onUpdateCallback(currentNode.id, 'update');
    }
  }

  // Zniszczenie panelu
  function destroy() {
    if (panelEl) panelEl.innerHTML = '';
    panelEl = null;
    currentNode = null;
  }

  return { init, showNode, showEmpty, destroy };
})();
