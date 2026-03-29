// =============================================================================
// Plik: modules/rules/TtsCleaningRules.js
// Opis: Widok zarzadzania regulami czyszczenia TTS - tabela CRUD, testowanie,
//       filtrowanie po typie, bulk import skrotow.
// Przyklad: ViewRouter.register('tts-rules', TtsCleaningRules);
// =============================================================================

const TtsCleaningRules = (() => {
  'use strict';

  let rulesList = [];
  let filterType = 'all';

  // Typy regul z etykietami
  const RULE_TYPES = {
    abbreviation: I18n.t('common.unknown').replace('Unknown', 'Abbreviation'),
    phonetic: I18n.t('common.unknown').replace('Unknown', 'Phonetic'),
    emoji_range: I18n.t('common.unknown').replace('Unknown', 'Emoji/Range'),
    regex_remove: 'Regex (remove)',
  };

  // Jezyki
  const LANGUAGES = {
    pl: 'Polski',
    en: 'English',
    de: 'Deutsch',
  };

  // Renderowanie HTML widoku
  function render() {
    const typeFilterOptions = Object.entries(RULE_TYPES).map(([val, label]) => {
      return `<option value="${val}">${label}</option>`;
    }).join('');

    return `
      <div class="rules-test-area">
        <h4 data-i18n="rules.tts.test_title">${I18n.t('rules.tts.test_title')}</h4>
        <div class="rules-test-row">
          <input type="text" id="tts-test-input" class="rules-test-input"
            placeholder="${I18n.t('rules.tts.test_hint')}" data-i18n-placeholder="rules.tts.test_hint">
          <button class="btn btn-primary btn-sm" id="btn-tts-test" data-i18n="rules.tts.test_btn">${I18n.t('rules.tts.test_btn')}</button>
        </div>
        <div id="tts-test-result" class="rules-test-result"></div>
      </div>

      <div class="rules-import-area">
        <h4 data-i18n="rules.tts.import_title">${I18n.t('rules.tts.import_title')}</h4>
        <textarea id="tts-import-textarea" class="rules-import-textarea"
          placeholder="Format: skrot${'\u2192'}pelna forma (jedna regula na linie)&#10;np.&#10;dr${'\u2192'}doktor&#10;ul.${'\u2192'}ulica&#10;mgr${'\u2192'}magister"></textarea>
        <div class="rules-import-hint">Separator: &rarr; (strzalka). Jedna regula na linie. Typ: abbreviation, jezyk: pl.</div>
        <div class="rules-import-actions">
          <button class="btn btn-primary btn-sm" id="btn-tts-import">Import</button>
        </div>
      </div>

      <div class="card">
        <div class="card-header">
          <h3 data-i18n="rules.tts.title">${I18n.t('rules.tts.title')}</h3>
          <div style="display: flex; gap: 8px; align-items: center;">
            <div class="rules-filter" style="margin-bottom: 0;">
              <label for="tts-filter-type">Filter:</label>
              <select id="tts-filter-type" style="min-width: 160px;">
                <option value="all" data-i18n="rules.tts.all_types">${I18n.t('rules.tts.all_types')}</option>
                ${typeFilterOptions}
              </select>
            </div>
            <button class="btn btn-primary btn-sm" id="btn-add-tts-rule" data-i18n="common.add">+ ${I18n.t('common.add')}</button>
          </div>
        </div>
        <div class="card-body no-padding">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th data-i18n="rules.tts.rule_type">${I18n.t('rules.tts.rule_type')}</th>
                  <th data-i18n="rules.pii.pattern">${I18n.t('rules.pii.pattern')}</th>
                  <th data-i18n="settings.value">${I18n.t('settings.value')}</th>
                  <th data-i18n="common.language">${I18n.t('common.language')}</th>
                  <th data-i18n="rules.pii.priority">${I18n.t('rules.pii.priority')}</th>
                  <th data-i18n="common.active">${I18n.t('common.active')}</th>
                  <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                </tr>
              </thead>
              <tbody id="tts-rules-tbody">
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
    `;
  }

  // Montowanie - zaladuj dane, podepnij zdarzenia
  function mount() {
    loadRules();

    document.getElementById('btn-add-tts-rule')?.addEventListener('click', () => openModal(null));
    document.getElementById('btn-tts-test')?.addEventListener('click', runTest);
    document.getElementById('btn-tts-import')?.addEventListener('click', importBulk);

    document.getElementById('tts-test-input')?.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') runTest();
    });

    document.getElementById('tts-filter-type')?.addEventListener('change', (e) => {
      filterType = e.target.value;
      renderTable();
    });

    // Delegacja zdarzen na tbody
    const tbody = document.getElementById('tts-rules-tbody');
    if (tbody) {
      tbody.addEventListener('click', handleTableClick);
    }
  }

  // Odmontowanie
  function unmount() {
    rulesList = [];
    filterType = 'all';
    closeModal();
  }

  // Zaladowanie regul z API
  async function loadRules() {
    try {
      rulesList = await ApiClient.get('/api/tts-rules');
      if (!Array.isArray(rulesList)) rulesList = [];
      renderTable();
    } catch (err) {
      console.error('Blad ladowania regul TTS:', err);
      rulesList = [];
      renderTable();
    }
  }

  // Pobranie przefiltrowanej listy
  function getFilteredRules() {
    if (filterType === 'all') return rulesList;
    return rulesList.filter(r => r.rule_type === filterType);
  }

  // Renderowanie tabeli
  function renderTable() {
    const tbody = document.getElementById('tts-rules-tbody');
    if (!tbody) return;

    const filtered = getFilteredRules();

    if (filtered.length === 0) {
      const hint = filterType !== 'all'
        ? `Brak reguł typu "${RULE_TYPES[filterType] || filterType}"`
        : I18n.t('common.no_data');
      tbody.innerHTML = `
        <tr>
          <td colspan="7">
            <div class="empty-state">
              <div class="empty-state-icon">&#128264;</div>
              <div class="empty-state-text">${hint}</div>
              <div class="empty-state-hint" data-i18n="rules.tts.empty_hint">${I18n.t('rules.tts.empty_hint')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = filtered.map(r => {
      const typeLabel = RULE_TYPES[r.rule_type] || r.rule_type;
      const langLabel = LANGUAGES[r.language] || r.language || 'pl';
      const activeClass = r.is_active ? 'is-active' : 'is-inactive';
      const activeLabel = r.is_active ? I18n.t('common.active') : I18n.t('common.inactive');

      return `
        <tr>
          <td><span class="rule-type-badge rule-type-${Utils.escapeAttr(r.rule_type)}">${Utils.escapeHtml(typeLabel)}</span></td>
          <td class="pattern-cell">${Utils.escapeHtml(r.pattern)}</td>
          <td>${Utils.escapeHtml(r.replacement || '')}</td>
          <td>${Utils.escapeHtml(langLabel)}</td>
          <td>${r.priority != null ? r.priority : 0}</td>
          <td>
            <span class="badge toggle-active ${activeClass}" data-tts-toggle="${r.id}">
              ${activeLabel}
            </span>
          </td>
          <td>
            <div style="display: flex; gap: 4px;">
              <button class="btn btn-ghost btn-sm" data-tts-edit="${r.id}" title="${I18n.t('common.edit')}" data-i18n-title="common.edit">&#9998;</button>
              <button class="btn btn-ghost btn-sm" data-tts-delete="${r.id}" title="${I18n.t('common.delete')}" data-i18n-title="common.delete">&#10005;</button>
            </div>
          </td>
        </tr>
      `;
    }).join('');

  }

  // Delegowany handler klikniec w tabeli
  function handleTableClick(e) {
    const toggleEl = e.target.closest('[data-tts-toggle]');
    if (toggleEl) {
      const id = parseInt(toggleEl.dataset.ttsToggle, 10);
      const rule = rulesList.find(r => r.id === id);
      if (rule) toggleActive(rule);
      return;
    }

    const editBtn = e.target.closest('[data-tts-edit]');
    if (editBtn) {
      const id = parseInt(editBtn.dataset.ttsEdit, 10);
      const rule = rulesList.find(r => r.id === id);
      if (rule) openModal(rule);
      return;
    }

    const deleteBtn = e.target.closest('[data-tts-delete]');
    if (deleteBtn) {
      const id = parseInt(deleteBtn.dataset.ttsDelete, 10);
      const rule = rulesList.find(r => r.id === id);
      if (rule) confirmDelete(rule);
    }
  }

  // Przelacz aktywnosc reguly
  async function toggleActive(rule) {
    try {
      const updated = Object.assign({}, rule, { is_active: !rule.is_active });
      await ApiClient.put('/api/tts-rules', updated);
      const statusLabel = updated.is_active ? I18n.t('rules.pii.activated') : I18n.t('rules.pii.deactivated');
      App.showToast(I18n.t('rules.tts.status_changed').replace('{status}', statusLabel), 'success');
      loadRules();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  // Potwierdzenie usuwania
  async function confirmDelete(rule) {
    if (!confirm(I18n.t('services.delete_confirm').replace('{name}', rule.pattern))) return;

    try {
      await ApiClient.delete(`/api/tts-rules/${rule.id}`);
      App.showToast(I18n.t('rules.tts.deleted'), 'success');
      loadRules();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  // Testowanie regul TTS na tekscie
  function runTest() {
    const input = document.getElementById('tts-test-input');
    const resultDiv = document.getElementById('tts-test-result');
    if (!input || !resultDiv) return;

    const text = input.value.trim();
    if (!text) {
      resultDiv.innerHTML = `<span style="color: var(--color-text-muted)">${I18n.t('rules.tts.test_hint')}</span>`;
      return;
    }

    // Posortuj aktywne reguly wg priorytetu malejaco
    const activeRules = rulesList
      .filter(r => r.is_active)
      .sort((a, b) => (b.priority || 0) - (a.priority || 0));

    if (activeRules.length === 0) {
      resultDiv.innerHTML = `<span style="color: var(--color-text-muted)">${I18n.t('rules.tts.no_active_rules')}</span>`;
      return;
    }

    let result = text;

    // Zastosuj kazda regule po kolei
    for (const rule of activeRules) {
      try {
        let regex;
        if (rule.rule_type === 'abbreviation') {
          // Dokladne dopasowanie slowa ze skrotem
          const escaped = rule.pattern.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
          regex = new RegExp('\\b' + escaped + '\\b', 'gi');
        } else {
          regex = new RegExp(rule.pattern, 'gi');
        }
        const replacement = rule.replacement || '';
        result = result.replace(regex, replacement);
      } catch (err) {
        // Pomijamy reguly z blednym regexem
      }
    }

    // Podswietl roznicy miedzy oryginalem a wynikiem
    if (result === text) {
      resultDiv.innerHTML = Utils.escapeHtml(result) + ' <span style="color: var(--color-text-muted)">(bez zmian)</span>';
    } else {
      resultDiv.innerHTML = buildDiffHtml(text, result);
    }
  }

  // Budowanie HTML z podswietlonymi zmianami
  function buildDiffHtml(original, cleaned) {
    // Prostsze podejscie: pokaz wynik z zaznaczeniem zmian
    // Porownanie slowo po slowie
    const origWords = original.split(/(\s+)/);
    const cleanWords = cleaned.split(/(\s+)/);

    let html = '';
    const maxLen = Math.max(origWords.length, cleanWords.length);

    for (let i = 0; i < maxLen; i++) {
      const ow = origWords[i] || '';
      const cw = cleanWords[i] || '';

      if (ow === cw) {
        html += Utils.escapeHtml(cw);
      } else if (cw) {
        html += `<span class="tts-change" title="Bylo: ${Utils.escapeAttr(ow)}">${Utils.escapeHtml(cw)}</span>`;
      }
    }

    if (!html.trim()) {
      html = '<span style="color: var(--color-text-muted)">(tekst został całkowicie usunięty)</span>';
    }

    return html;
  }

  // Import bulk skrotow
  async function importBulk() {
    const textarea = document.getElementById('tts-import-textarea');
    if (!textarea) return;

    const text = textarea.value.trim();
    if (!text) {
      App.showToast(I18n.t('rules.tts.import_empty'), 'error');
      return;
    }

    const lines = text.split('\n').filter(l => l.trim());
    const rules = [];

    for (const line of lines) {
      // Separator: strzalka unicode lub ->
      const sep = line.includes('\u2192') ? '\u2192' : '->';
      const parts = line.split(sep);
      if (parts.length < 2) continue;

      const pattern = parts[0].trim();
      const replacement = parts.slice(1).join(sep).trim();
      if (!pattern) continue;

      rules.push({
        rule_type: 'abbreviation',
        pattern,
        replacement,
        language: 'pl',
        priority: 50,
        is_active: true,
      });
    }

    if (rules.length === 0) {
      App.showToast(I18n.t('rules.tts.import_invalid'), 'error');
      return;
    }

    let created = 0;
    let errors = 0;

    // Rownolegly import w partiach po 5
    const BATCH_SIZE = 5;
    for (let i = 0; i < rules.length; i += BATCH_SIZE) {
      const batch = rules.slice(i, i + BATCH_SIZE);
      const results = await Promise.allSettled(
        batch.map(rule => ApiClient.post('/api/tts-rules', rule))
      );
      for (const r of results) {
        if (r.status === 'fulfilled') created++;
        else errors++;
      }
    }

    if (errors > 0) {
      App.showToast(I18n.t('rules.tts.import_warn').replace('{created}', created).replace('{errors}', errors), 'warning');
    } else {
      App.showToast(I18n.t('rules.tts.import_success').replace('{created}', created), 'success');
    }

    textarea.value = '';
    loadRules();
  }

  // Otworz modal formularza
  function openModal(rule) {
    closeModal();

    const isEdit = !!rule;
    const title = isEdit ? I18n.t('common.edit') : I18n.t('common.add');

    const ruleTypeOptions = Object.entries(RULE_TYPES).map(([val, label]) => {
      const sel = rule && rule.rule_type === val ? 'selected' : '';
      return `<option value="${val}" ${sel}>${label}</option>`;
    }).join('');

    const languageOptions = Object.entries(LANGUAGES).map(([val, label]) => {
      const sel = rule && rule.language === val ? 'selected' : (val === 'pl' && !rule ? 'selected' : '');
      return `<option value="${val}" ${sel}>${label}</option>`;
    }).join('');

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.id = 'tts-modal-overlay';
    overlay.innerHTML = `
      <div class="modal" style="max-width: 520px;">
        <div class="modal-header">
          <h3>${title}</h3>
          <button class="modal-close" id="tts-modal-close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="form-group">
            <label for="tts-rule-type" data-i18n="rules.tts.rule_type">${I18n.t('rules.tts.rule_type')}</label>
            <select id="tts-rule-type">
              ${ruleTypeOptions}
            </select>
          </div>
          <div class="form-group">
            <label for="tts-pattern" data-i18n="rules.pii.pattern">${I18n.t('rules.pii.pattern')}</label>
            <input type="text" id="tts-pattern" value="${Utils.escapeAttr(rule?.pattern || '')}"
              placeholder="np. dr lub [\\p{Emoji}]"
              style="font-family: 'JetBrains Mono', monospace;">
            <div id="tts-pattern-error" class="field-error-msg" hidden></div>
          </div>
          <div class="form-group">
            <label for="tts-replacement">Zamiennik</label>
            <input type="text" id="tts-replacement" value="${Utils.escapeAttr(rule?.replacement || '')}"
              placeholder="Zamiennik (pusty dla regex_remove)">
          </div>
          <div class="form-group">
            <label for="tts-language">Język</label>
            <select id="tts-language">
              ${languageOptions}
            </select>
          </div>
          <div class="form-group">
            <label for="tts-priority" data-i18n="rules.pii.priority">${I18n.t('rules.pii.priority')}</label>
            <input type="number" id="tts-priority" min="0" max="100"
              value="${rule?.priority != null ? rule.priority : 50}">
          </div>
          <div class="form-group">
            <label style="display: flex; align-items: center; gap: 8px; cursor: pointer;">
              <input type="checkbox" id="tts-is-active" ${rule?.is_active !== false ? 'checked' : ''}
                style="width: auto;">
              <span data-i18n="common.active">${I18n.t('common.active')}</span>
            </label>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-ghost" id="tts-modal-cancel" data-i18n="common.cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="tts-modal-save" data-i18n="common.save">${I18n.t('common.save')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    // Zdarzenia modalu
    overlay.querySelector('#tts-modal-close').addEventListener('click', closeModal);
    overlay.querySelector('#tts-modal-cancel').addEventListener('click', closeModal);
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) closeModal();
    });

    // Walidacja regex dla typow regex_remove i emoji_range
    overlay.querySelector('#tts-pattern').addEventListener('input', (e) => {
      const type = document.getElementById('tts-rule-type')?.value;
      if (type === 'regex_remove' || type === 'emoji_range') {
        validateRegex(e.target.value);
      }
    });

    overlay.querySelector('#tts-rule-type').addEventListener('change', () => {
      const type = document.getElementById('tts-rule-type')?.value;
      const patternInput = document.getElementById('tts-pattern');
      const errorDiv = document.getElementById('tts-pattern-error');
      if (type === 'regex_remove' || type === 'emoji_range') {
        if (patternInput?.value) validateRegex(patternInput.value);
      } else {
        if (errorDiv) errorDiv.hidden = true;
        patternInput?.classList.remove('field-error');
      }
    });

    // Zapis
    overlay.querySelector('#tts-modal-save').addEventListener('click', () => saveRule(rule));
  }

  // Zamknij modal
  function closeModal() {
    const overlay = document.getElementById('tts-modal-overlay');
    if (overlay) overlay.remove();
  }

  // Walidacja regex
  function validateRegex(pattern) {
    const errorDiv = document.getElementById('tts-pattern-error');
    if (!errorDiv) return true;

    if (!pattern) {
      errorDiv.hidden = true;
      return true;
    }

    try {
      new RegExp(pattern);
      errorDiv.hidden = true;
      document.getElementById('tts-pattern')?.classList.remove('field-error');
      return true;
    } catch (err) {
      errorDiv.textContent = `${I18n.t('rules.tts.pattern_invalid')}: ${err.message}`;
      errorDiv.hidden = false;
      document.getElementById('tts-pattern')?.classList.add('field-error');
      return false;
    }
  }

  // Zapis reguly (tworzenie lub aktualizacja)
  async function saveRule(existingRule) {
    const ruleType = document.getElementById('tts-rule-type')?.value;
    const pattern = document.getElementById('tts-pattern')?.value.trim();
    const replacement = document.getElementById('tts-replacement')?.value || '';
    const language = document.getElementById('tts-language')?.value || 'pl';
    const priority = parseInt(document.getElementById('tts-priority')?.value || '50', 10);
    const isActive = document.getElementById('tts-is-active')?.checked ?? true;

    // Walidacja
    if (!pattern) {
      App.showToast(I18n.t('rules.tts.pattern_required'), 'error');
      return;
    }

    // Walidacja regex dla typow ktore tego wymagaja
    if ((ruleType === 'regex_remove' || ruleType === 'emoji_range') && !validateRegex(pattern)) {
      App.showToast(I18n.t('rules.tts.pattern_invalid'), 'error');
      return;
    }

    const data = {
      rule_type: ruleType,
      pattern,
      replacement,
      language,
      priority,
      is_active: isActive,
    };

    try {
      if (existingRule) {
        data.id = existingRule.id;
        await ApiClient.put('/api/tts-rules', data);
        App.showToast(I18n.t('rules.tts.updated'), 'success');
      } else {
        await ApiClient.post('/api/tts-rules', data);
        App.showToast(I18n.t('rules.tts.created'), 'success');
      }
      closeModal();
      loadRules();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  return { render, mount, unmount };
})();
