// =============================================================================
// Plik: modules/rules/FastPathPatterns.js
// Opis: Widok zarzadzania wzorcami fast-path - dwie sekcje (Intent Analyzer,
//       Memory Analyzer), tabele CRUD, badge typow.
// Przyklad: ViewRouter.register('fast-path', FastPathPatterns);
// =============================================================================

const FastPathPatterns = (() => {
  'use strict';

  let patternsList = [];

  // Typy wzorcow z etykietami
  const PATTERN_TYPES = {
    greeting: I18n.t('rules.fast_path.pattern_types.greeting'),
    farewell: I18n.t('rules.fast_path.pattern_types.farewell'),
    question_to_ai: I18n.t('rules.fast_path.pattern_types.question_to_ai'),
    introduction: I18n.t('rules.fast_path.pattern_types.introduction'),
    short_message: I18n.t('rules.fast_path.pattern_types.short_message'),
    name_correction: I18n.t('rules.fast_path.pattern_types.name_correction'),
    conversation: I18n.t('playground.conversation'),
  };

  // Typy dopasowania z etykietami
  const MATCH_TYPES = {
    exact: I18n.t('rules.fast_path.match_types.exact'),
    starts_with: I18n.t('rules.fast_path.match_types.starts_with'),
    contains: I18n.t('rules.fast_path.match_types.contains'),
    regex: I18n.t('rules.fast_path.match_types.regex'),
    length: I18n.t('rules.fast_path.match_types.length'),
  };

  // Moduly z etykietami
  const MODULES = {
    intent_analyzer: 'Intent Analyzer',
    memory_analyzer: 'Memory Analyzer',
  };

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="rules-section">
        <div class="rules-section-header">
          <h3 class="rules-section-title" data-i18n="rules.fast_path.intent_title">${I18n.t('rules.fast_path.intent_title')}</h3>
          <button class="btn btn-primary btn-sm" data-add-module="intent_analyzer" data-i18n="common.add">+ ${I18n.t('common.add')}</button>
        </div>
        <div class="card">
          <div class="card-body no-padding">
            <div class="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th data-i18n="rules.fast_path.pattern_type">${I18n.t('rules.fast_path.pattern_type')}</th>
                    <th data-i18n="rules.pii.pattern">${I18n.t('rules.pii.pattern')}</th>
                    <th data-i18n="rules.fast_path.match_type">${I18n.t('rules.fast_path.match_type')}</th>
                    <th data-i18n="rules.fast_path.result_json">${I18n.t('rules.fast_path.result_json')}</th>
                    <th data-i18n="rules.pii.priority">${I18n.t('rules.pii.priority')}</th>
                    <th data-i18n="common.active">${I18n.t('common.active')}</th>
                    <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                  </tr>
                </thead>
                <tbody id="fp-intent-tbody">
                  <tr>
                    <td colspan="7">
                      <div class="empty-state">
                        <div class="empty-state-text" data-i18n="common.loading">${I18n.t('common.loading')}</div>
                      </div>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </div>
        </div>
      </div>

      <div class="rules-section">
        <div class="rules-section-header">
          <h3 class="rules-section-title" data-i18n="rules.fast_path.memory_title">${I18n.t('rules.fast_path.memory_title')}</h3>
          <button class="btn btn-primary btn-sm" data-add-module="memory_analyzer" data-i18n="common.add">+ ${I18n.t('common.add')}</button>
        </div>
        <div class="card">
          <div class="card-body no-padding">
            <div class="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th data-i18n="rules.fast_path.pattern_type">${I18n.t('rules.fast_path.pattern_type')}</th>
                    <th data-i18n="rules.pii.pattern">${I18n.t('rules.pii.pattern')}</th>
                    <th data-i18n="rules.fast_path.match_type">${I18n.t('rules.fast_path.match_type')}</th>
                    <th data-i18n="rules.fast_path.result_json">${I18n.t('rules.fast_path.result_json')}</th>
                    <th data-i18n="rules.pii.priority">${I18n.t('rules.pii.priority')}</th>
                    <th data-i18n="common.active">${I18n.t('common.active')}</th>
                    <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                  </tr>
                </thead>
                <tbody id="fp-memory-tbody">
                  <tr>
                    <td colspan="7">
                      <div class="empty-state">
                        <div class="empty-state-text" data-i18n="common.loading">${I18n.t('common.loading')}</div>
                      </div>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </div>
        </div>
      </div>
    `;
  }

  // Montowanie - zaladuj dane, podepnij zdarzenia
  function mount() {
    loadPatterns();

    document.querySelectorAll('[data-add-module]').forEach(btn => {
      btn.addEventListener('click', () => {
        openModal(null, btn.dataset.addModule);
      });
    });

    // Delegacja zdarzen na obie tabele
    const intentTbody = document.getElementById('fp-intent-tbody');
    if (intentTbody) {
      intentTbody.addEventListener('click', handleTableClick);
    }
    const memoryTbody = document.getElementById('fp-memory-tbody');
    if (memoryTbody) {
      memoryTbody.addEventListener('click', handleTableClick);
    }
  }

  // Odmontowanie
  function unmount() {
    patternsList = [];
    closeModal();
  }

  // Zaladowanie wzorcow z API
  async function loadPatterns() {
    try {
      patternsList = await ApiClient.get('/api/fast-path');
      if (!Array.isArray(patternsList)) patternsList = [];
      renderTables();
    } catch (err) {
      console.error('Blad ladowania wzorcow fast-path:', err);
      patternsList = [];
      renderTables();
    }
  }

  // Renderowanie obu tabel
  function renderTables() {
    const intentPatterns = patternsList.filter(p => p.module === 'intent_analyzer');
    const memoryPatterns = patternsList.filter(p => p.module === 'memory_analyzer');

    renderSectionTable('fp-intent-tbody', intentPatterns);
    renderSectionTable('fp-memory-tbody', memoryPatterns);
  }

  // Renderowanie jednej tabeli sekcji
  function renderSectionTable(tbodyId, patterns) {
    const tbody = document.getElementById(tbodyId);
    if (!tbody) return;

    if (patterns.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="7">
            <div class="empty-state">
              <div class="empty-state-icon">&#9889;</div>
              <div class="empty-state-text" data-i18n="rules.fast_path.empty">${I18n.t('rules.fast_path.empty')}</div>
              <div class="empty-state-hint" data-i18n="rules.fast_path.empty_hint">${I18n.t('rules.fast_path.empty_hint')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = patterns.map(p => {
      const ptLabel = PATTERN_TYPES[p.pattern_type] || p.pattern_type;
      const mtLabel = MATCH_TYPES[p.match_type] || p.match_type;
      const activeClass = p.is_active ? 'is-active' : 'is-inactive';
      const activeLabel = p.is_active ? I18n.t('common.active') : I18n.t('common.inactive');

      let resultDisplay = '';
      if (p.result_json) {
        try {
          const parsed = typeof p.result_json === 'string' ? JSON.parse(p.result_json) : p.result_json;
          resultDisplay = JSON.stringify(parsed);
        } catch {
          resultDisplay = Utils.escapeHtml(String(p.result_json));
        }
      }

      return `
        <tr>
          <td><span class="pattern-type-badge pattern-type-${Utils.escapeAttr(p.pattern_type)}">${Utils.escapeHtml(ptLabel)}</span></td>
          <td class="pattern-cell">${Utils.escapeHtml(p.pattern)}</td>
          <td><span class="match-badge match-badge-${Utils.escapeAttr(p.match_type)}">${Utils.escapeHtml(mtLabel)}</span></td>
          <td class="json-cell" title="${Utils.escapeAttr(resultDisplay)}">${Utils.escapeHtml(resultDisplay)}</td>
          <td>${p.priority != null ? p.priority : 0}</td>
          <td>
            <span class="badge toggle-active ${activeClass}" data-fp-toggle="${p.id}">
              ${activeLabel}
            </span>
          </td>
          <td>
            <div style="display: flex; gap: 4px;">
              <button class="btn btn-ghost btn-sm" data-fp-edit="${p.id}" title="${I18n.t('common.edit')}" data-i18n-title="common.edit">&#9998;</button>
              <button class="btn btn-ghost btn-sm" data-fp-delete="${p.id}" title="${I18n.t('common.delete')}" data-i18n-title="common.delete">&#10005;</button>
            </div>
          </td>
        </tr>
      `;
    }).join('');

  }

  // Delegowany handler klikniec w tabelach
  function handleTableClick(e) {
    const toggleEl = e.target.closest('[data-fp-toggle]');
    if (toggleEl) {
      const id = parseInt(toggleEl.dataset.fpToggle, 10);
      const pattern = patternsList.find(p => p.id === id);
      if (pattern) toggleActive(pattern);
      return;
    }

    const editBtn = e.target.closest('[data-fp-edit]');
    if (editBtn) {
      const id = parseInt(editBtn.dataset.fpEdit, 10);
      const pattern = patternsList.find(p => p.id === id);
      if (pattern) openModal(pattern, pattern.module);
      return;
    }

    const deleteBtn = e.target.closest('[data-fp-delete]');
    if (deleteBtn) {
      const id = parseInt(deleteBtn.dataset.fpDelete, 10);
      const pattern = patternsList.find(p => p.id === id);
      if (pattern) confirmDelete(pattern);
    }
  }

  // Przelacz aktywnosc wzorca
  async function toggleActive(pattern) {
    try {
      const updated = Object.assign({}, pattern, { is_active: !pattern.is_active });
      await ApiClient.put('/api/fast-path', updated);
      const statusLabel = updated.is_active ? I18n.t('rules.pii.activated') : I18n.t('rules.pii.deactivated');
      App.showToast(I18n.t('rules.fast_path.status_changed').replace('{status}', statusLabel), 'success');
      loadPatterns();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  // Potwierdzenie usuwania
  async function confirmDelete(pattern) {
    if (!confirm(I18n.t('services.delete_confirm').replace('{name}', pattern.pattern))) return;

    try {
      await ApiClient.delete(`/api/fast-path/${pattern.id}`);
      App.showToast(I18n.t('rules.fast_path.deleted'), 'success');
      loadPatterns();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  // Otworz modal formularza
  function openModal(pattern, moduleName) {
    closeModal();

    const isEdit = !!pattern;
    const title = isEdit ? 'Edytuj wzorzec fast-path' : `Nowy wzorzec - ${MODULES[moduleName] || moduleName}`;

    const patternTypeOptions = Object.entries(PATTERN_TYPES).map(([val, label]) => {
      const sel = pattern && pattern.pattern_type === val ? 'selected' : '';
      return `<option value="${val}" ${sel}>${label}</option>`;
    }).join('');

    const matchTypeOptions = Object.entries(MATCH_TYPES).map(([val, label]) => {
      const sel = pattern && pattern.match_type === val ? 'selected' : '';
      return `<option value="${val}" ${sel}>${label}</option>`;
    }).join('');

    let resultJsonStr = '';
    if (pattern?.result_json) {
      try {
        const parsed = typeof pattern.result_json === 'string' ? JSON.parse(pattern.result_json) : pattern.result_json;
        resultJsonStr = JSON.stringify(parsed, null, 2);
      } catch {
        resultJsonStr = String(pattern.result_json);
      }
    }

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.id = 'fp-modal-overlay';
    overlay.innerHTML = `
      <div class="modal" style="max-width: 580px;">
        <div class="modal-header">
          <h3>${title}</h3>
          <button class="modal-close" id="fp-modal-close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="form-group">
            <label for="fp-pattern-type">Typ wzorca</label>
            <select id="fp-pattern-type">
              ${patternTypeOptions}
            </select>
          </div>
          <div class="form-group">
            <label for="fp-pattern">Wzorzec</label>
            <input type="text" id="fp-pattern" value="${Utils.escapeAttr(pattern?.pattern || '')}"
              placeholder="np. czesc, hej, witam">
          </div>
          <div class="form-group">
            <label for="fp-match-type">Typ dopasowania</label>
            <select id="fp-match-type">
              ${matchTypeOptions}
            </select>
          </div>
          <div class="form-group">
            <label for="fp-result-json">Wynik JSON</label>
            <textarea id="fp-result-json"
              style="font-family: 'JetBrains Mono', monospace; min-height: 100px;"
              placeholder='{"intent": "greeting", "confidence": 1.0}'>${Utils.escapeHtml(resultJsonStr)}</textarea>
            <div id="fp-json-error" class="field-error-msg" hidden></div>
          </div>
          <div class="form-group">
            <label for="fp-priority">Priorytet</label>
            <input type="number" id="fp-priority" min="0" max="100"
              value="${pattern?.priority != null ? pattern.priority : 50}">
          </div>
          <div class="form-group">
            <label style="display: flex; align-items: center; gap: 8px; cursor: pointer;">
              <input type="checkbox" id="fp-is-active" ${pattern?.is_active !== false ? 'checked' : ''}
                style="width: auto;">
              Aktywny
            </label>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-ghost" id="fp-modal-cancel" data-i18n="common.cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="fp-modal-save">${isEdit ? I18n.t('common.save') : I18n.t('common.add')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    // Zdarzenia modalu
    overlay.querySelector('#fp-modal-close').addEventListener('click', closeModal);
    overlay.querySelector('#fp-modal-cancel').addEventListener('click', closeModal);
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) closeModal();
    });

    // Walidacja JSON na biezaco
    overlay.querySelector('#fp-result-json').addEventListener('input', (e) => {
      validateJson(e.target.value);
    });

    // Zapis
    overlay.querySelector('#fp-modal-save').addEventListener('click', () => savePattern(pattern, moduleName));
  }

  // Zamknij modal
  function closeModal() {
    const overlay = document.getElementById('fp-modal-overlay');
    if (overlay) overlay.remove();
  }

  // Walidacja JSON
  function validateJson(str) {
    const errorDiv = document.getElementById('fp-json-error');
    if (!errorDiv) return true;

    if (!str || !str.trim()) {
      errorDiv.hidden = true;
      document.getElementById('fp-result-json')?.classList.remove('field-error');
      return true;
    }

    try {
      JSON.parse(str);
      errorDiv.hidden = true;
      document.getElementById('fp-result-json')?.classList.remove('field-error');
      return true;
    } catch (err) {
      errorDiv.textContent = `Nieprawidłowy JSON: ${err.message}`;
      errorDiv.hidden = false;
      document.getElementById('fp-result-json')?.classList.add('field-error');
      return false;
    }
  }

  // Zapis wzorca (tworzenie lub aktualizacja)
  async function savePattern(existingPattern, moduleName) {
    const patternType = document.getElementById('fp-pattern-type')?.value;
    const pattern = document.getElementById('fp-pattern')?.value.trim();
    const matchType = document.getElementById('fp-match-type')?.value;
    const resultJsonStr = document.getElementById('fp-result-json')?.value.trim();
    const priority = parseInt(document.getElementById('fp-priority')?.value || '50', 10);
    const isActive = document.getElementById('fp-is-active')?.checked ?? true;

    // Walidacja
    if (!pattern) {
      App.showToast('Wzorzec jest wymagany', 'error');
      return;
    }
    if (resultJsonStr && !validateJson(resultJsonStr)) {
      App.showToast('Wynik JSON musi być poprawnym JSON', 'error');
      return;
    }

    let resultJson = null;
    if (resultJsonStr) {
      try { resultJson = JSON.parse(resultJsonStr); } catch { resultJson = null; }
    }

    const data = {
      module: moduleName,
      pattern_type: patternType,
      pattern,
      match_type: matchType,
      result_json: resultJson,
      priority,
      is_active: isActive,
    };

    try {
      if (existingPattern) {
        data.id = existingPattern.id;
        await ApiClient.put('/api/fast-path', data);
        App.showToast('Wzorzec zaktualizowany', 'success');
      } else {
        await ApiClient.post('/api/fast-path', data);
        App.showToast('Wzorzec utworzony', 'success');
      }
      closeModal();
      loadPatterns();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  return { render, mount, unmount };
})();
