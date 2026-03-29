// =============================================================================
// Plik: modules/rules/PiiRules.js
// Opis: Widok zarzadzania regulami PII - tabela CRUD, testowanie regex,
//       toggle aktywnosci, badge kategorii.
// Przyklad: ViewRouter.register('pii-rules', PiiRules);
// =============================================================================

const PiiRules = (() => {
  'use strict';

  let rulesList = [];

  // Kategorie PII z etykietami
  const CATEGORIES = {
    tax_id: I18n.t('rules.pii.categories.tax_id'),
    personal_id: I18n.t('rules.pii.categories.personal_id'),
    email: I18n.t('rules.pii.categories.email'),
    phone: I18n.t('rules.pii.categories.phone'),
    address: I18n.t('rules.pii.categories.address'),
    name: I18n.t('rules.pii.categories.name'),
    custom: I18n.t('rules.pii.categories.custom'),
  };

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="rules-test-area">
        <h4 data-i18n="rules.pii.test_title">${I18n.t('rules.pii.test_title')}</h4>
        <div class="rules-test-row">
          <input type="text" id="pii-test-input" class="rules-test-input"
            placeholder="${I18n.t('rules.pii.test_hint')}" data-i18n-placeholder="rules.pii.test_hint">
          <button class="btn btn-primary btn-sm" id="btn-pii-test" data-i18n="settings.portainer.test">${I18n.t('settings.portainer.test')}</button>
        </div>
        <div id="pii-test-result" class="rules-test-result"></div>
      </div>

      <div class="card">
        <div class="card-header">
          <h3 data-i18n="rules.pii.title">${I18n.t('rules.pii.title')}</h3>
          <button class="btn btn-primary btn-sm" id="btn-add-pii-rule" data-i18n="common.add">+ ${I18n.t('common.add')}</button>
        </div>
        <div class="card-body no-padding">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th data-i18n="common.name">${I18n.t('common.name')}</th>
                  <th>Kategoria</th>
                  <th data-i18n="rules.pii.pattern">${I18n.t('rules.pii.pattern')}</th>
                  <th>Zamiennik</th>
                  <th data-i18n="rules.pii.priority">${I18n.t('rules.pii.priority')}</th>
                  <th data-i18n="common.active">${I18n.t('common.active')}</th>
                  <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                </tr>
              </thead>
              <tbody id="pii-rules-tbody">
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

    document.getElementById('btn-add-pii-rule')?.addEventListener('click', () => openModal(null));
    document.getElementById('btn-pii-test')?.addEventListener('click', runTest);
    document.getElementById('pii-test-input')?.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') runTest();
    });

    // Delegacja zdarzen na tbody
    const tbody = document.getElementById('pii-rules-tbody');
    if (tbody) {
      tbody.addEventListener('click', handleTableClick);
    }
  }

  // Odmontowanie
  function unmount() {
    rulesList = [];
    closeModal();
  }

  // Zaladowanie regul z API
  async function loadRules() {
    try {
      rulesList = await ApiClient.get('/api/pii-rules');
      if (!Array.isArray(rulesList)) rulesList = [];
      renderTable();
    } catch (err) {
      console.error('Blad ladowania regul PII:', err);
      rulesList = [];
      renderTable();
    }
  }

  // Renderowanie tabeli
  function renderTable() {
    const tbody = document.getElementById('pii-rules-tbody');
    if (!tbody) return;

    if (rulesList.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="7">
            <div class="empty-state">
              <div class="empty-state-icon">&#128274;</div>
              <div class="empty-state-text" data-i18n="rules.pii.empty">${I18n.t('rules.pii.empty')}</div>
              <div class="empty-state-hint" data-i18n="rules.pii.empty_hint">${I18n.t('rules.pii.empty_hint')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = rulesList.map(r => {
      const catLabel = CATEGORIES[r.category] || r.category;
      const activeClass = r.is_active ? 'is-active' : 'is-inactive';
      const activeLabel = r.is_active ? I18n.t('common.active') : I18n.t('common.inactive');
      return `
        <tr>
          <td><strong>${Utils.escapeHtml(r.name)}</strong></td>
          <td><span class="rule-badge rule-badge-${Utils.escapeAttr(r.category)}">${Utils.escapeHtml(catLabel)}</span></td>
          <td class="pattern-cell">${Utils.escapeHtml(r.pattern)}</td>
          <td>${Utils.escapeHtml(r.replacement || '[UKRYTY]')}</td>
          <td>${r.priority != null ? r.priority : 0}</td>
          <td>
            <span class="badge toggle-active ${activeClass}" data-toggle-id="${r.id}">
              ${activeLabel}
            </span>
          </td>
          <td>
            <div style="display: flex; gap: 4px;">
              <button class="btn btn-ghost btn-sm" data-edit-pii="${r.id}" title="${I18n.t('common.edit')}" data-i18n-title="common.edit">&#9998;</button>
              <button class="btn btn-ghost btn-sm" data-delete-pii="${r.id}" title="${I18n.t('common.delete')}" data-i18n-title="common.delete">&#10005;</button>
            </div>
          </td>
        </tr>
      `;
    }).join('');

  }

  // Delegowany handler klikniec w tabeli
  function handleTableClick(e) {
    const toggleEl = e.target.closest('[data-toggle-id]');
    if (toggleEl) {
      const id = parseInt(toggleEl.dataset.toggleId, 10);
      const rule = rulesList.find(r => r.id === id);
      if (rule) toggleActive(rule);
      return;
    }

    const editBtn = e.target.closest('[data-edit-pii]');
    if (editBtn) {
      const id = parseInt(editBtn.dataset.editPii, 10);
      const rule = rulesList.find(r => r.id === id);
      if (rule) openModal(rule);
      return;
    }

    const deleteBtn = e.target.closest('[data-delete-pii]');
    if (deleteBtn) {
      const id = parseInt(deleteBtn.dataset.deletePii, 10);
      const rule = rulesList.find(r => r.id === id);
      if (rule) confirmDelete(rule);
    }
  }

  // Przelacz aktywnosc reguly
  async function toggleActive(rule) {
    try {
      const updated = Object.assign({}, rule, { is_active: !rule.is_active });
      await ApiClient.put('/api/pii-rules', updated);
      const statusLabel = updated.is_active ? I18n.t('rules.pii.activated') : I18n.t('rules.pii.deactivated');
      App.showToast(I18n.t('rules.pii.status_changed').replace('{name}', rule.name).replace('{status}', statusLabel), 'success');
      loadRules();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  // Potwierdzenie usuwania
  async function confirmDelete(rule) {
    if (!confirm(I18n.t('services.delete_confirm').replace('{name}', rule.name))) return;

    try {
      await ApiClient.delete(`/api/pii-rules/${rule.id}`);
      App.showToast(I18n.t('rules.pii.deleted').replace('{name}', rule.name), 'success');
      loadRules();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  // Testowanie regul PII na tekscie
  function runTest() {
    const input = document.getElementById('pii-test-input');
    const resultDiv = document.getElementById('pii-test-result');
    if (!input || !resultDiv) return;

    const text = input.value.trim();
    if (!text) {
      resultDiv.innerHTML = `<span style="color: var(--color-text-muted)">${I18n.t('rules.pii.test_hint')}</span>`;
      return;
    }

    // Posortuj aktywne reguly wg priorytetu malejaco
    const activeRules = rulesList
      .filter(r => r.is_active)
      .sort((a, b) => (b.priority || 0) - (a.priority || 0));

    if (activeRules.length === 0) {
      resultDiv.innerHTML = `<span style="color: var(--color-text-muted)">${I18n.t('rules.pii.no_active_rules')}</span>`;
      return;
    }

    let result = Utils.escapeHtml(text);

    // Zastosuj kazda regule po kolei
    for (const rule of activeRules) {
      try {
        const regex = new RegExp(rule.pattern, 'g');
        const replacement = rule.replacement || '[UKRYTY]';
        result = result.replace(regex, (match) => {
          return `<span class="pii-match" title="${Utils.escapeAttr(rule.name)}">${Utils.escapeHtml(replacement)}</span>`;
        });
      } catch (err) {
        // Pomijamy reguly z blednym regexem
      }
    }

    resultDiv.innerHTML = result;
  }

  // Otworz modal formularza
  function openModal(rule) {
    closeModal();

    const isEdit = !!rule;
    const title = isEdit ? I18n.t('common.edit') : I18n.t('common.add');

    const categoryOptions = Object.entries(CATEGORIES).map(([val, label]) => {
      const sel = rule && rule.category === val ? 'selected' : '';
      return `<option value="${val}" ${sel}>${label}</option>`;
    }).join('');

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.id = 'pii-modal-overlay';
    overlay.innerHTML = `
      <div class="modal" style="max-width: 580px;">
        <div class="modal-header">
          <h3>${title}</h3>
          <button class="modal-close" id="pii-modal-close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="form-group">
            <label for="pii-name" data-i18n="common.name">${I18n.t('common.name')}</label>
            <input type="text" id="pii-name" value="${Utils.escapeAttr(rule?.name || '')}" placeholder="np. NIP">
          </div>
          <div class="form-group">
            <label for="pii-category">Kategoria</label>
            <select id="pii-category">
              ${categoryOptions}
            </select>
          </div>
          <div class="form-group">
            <label for="pii-pattern" data-i18n="rules.pii.pattern">${I18n.t('rules.pii.pattern')}</label>
            <input type="text" id="pii-pattern" value="${Utils.escapeAttr(rule?.pattern || '')}"
              placeholder="np. \\b\\d{10}\\b" style="font-family: 'JetBrains Mono', monospace;">
            <div id="pii-pattern-error" class="field-error-msg" hidden></div>
          </div>
          <div class="form-group">
            <label for="pii-replacement">Zamiennik</label>
            <input type="text" id="pii-replacement" value="${Utils.escapeAttr(rule?.replacement || '[UKRYTY]')}"
              placeholder="[UKRYTY]">
          </div>
          <div class="form-group">
            <label for="pii-priority" data-i18n="rules.pii.priority">${I18n.t('rules.pii.priority')}</label>
            <input type="number" id="pii-priority" min="0" max="100"
              value="${rule?.priority != null ? rule.priority : 50}">
          </div>
          <div class="form-group">
            <label for="pii-description" data-i18n="common.description">${I18n.t('common.description')}</label>
            <textarea id="pii-description">${Utils.escapeHtml(rule?.description || '')}</textarea>
          </div>
          <div class="form-group">
            <label for="pii-test-examples" data-i18n="rules.pii.test_examples">${I18n.t('rules.pii.test_examples')}</label>
            <textarea id="pii-test-examples"
              style="font-family: 'JetBrains Mono', monospace; min-height: 80px;"
              placeholder='[{"input":"NIP 1234567890","expected":"NIP [NIP]"}]'>${Utils.escapeHtml(rule?.test_examples ? JSON.stringify(rule.test_examples, null, 2) : '')}</textarea>
            <div id="pii-examples-error" class="field-error-msg" hidden></div>
          </div>
          <div class="form-group">
            <label style="display: flex; align-items: center; gap: 8px; cursor: pointer;">
              <input type="checkbox" id="pii-is-active" ${rule?.is_active !== false ? 'checked' : ''}
                style="width: auto;">
              <span data-i18n="common.active">${I18n.t('common.active')}</span>
            </label>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-ghost" id="pii-modal-cancel" data-i18n="common.cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="pii-modal-save" data-i18n="common.save">${I18n.t('common.save')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    // Zdarzenia modalu
    overlay.querySelector('#pii-modal-close').addEventListener('click', closeModal);
    overlay.querySelector('#pii-modal-cancel').addEventListener('click', closeModal);
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) closeModal();
    });

    // Walidacja regex na biezaco
    overlay.querySelector('#pii-pattern').addEventListener('input', (e) => {
      validateRegex(e.target.value);
    });

    // Zapis
    overlay.querySelector('#pii-modal-save').addEventListener('click', () => saveRule(rule));
  }

  // Zamknij modal
  function closeModal() {
    const overlay = document.getElementById('pii-modal-overlay');
    if (overlay) overlay.remove();
  }

  // Walidacja regex
  function validateRegex(pattern) {
    const errorDiv = document.getElementById('pii-pattern-error');
    if (!errorDiv) return true;

    if (!pattern) {
      errorDiv.hidden = true;
      return true;
    }

    try {
      new RegExp(pattern);
      errorDiv.hidden = true;
      document.getElementById('pii-pattern')?.classList.remove('field-error');
      return true;
    } catch (err) {
      errorDiv.textContent = `${I18n.t('rules.pii.pattern_invalid')}: ${err.message}`;
      errorDiv.hidden = false;
      document.getElementById('pii-pattern')?.classList.add('field-error');
      return false;
    }
  }

  // Walidacja JSON
  function validateJson(str) {
    if (!str || !str.trim()) return true;
    try {
      JSON.parse(str);
      return true;
    } catch {
      return false;
    }
  }

  // Zapis reguly (tworzenie lub aktualizacja)
  async function saveRule(existingRule) {
    const name = document.getElementById('pii-name')?.value.trim();
    const category = document.getElementById('pii-category')?.value;
    const pattern = document.getElementById('pii-pattern')?.value.trim();
    const replacement = document.getElementById('pii-replacement')?.value || '[UKRYTY]';
    const priority = parseInt(document.getElementById('pii-priority')?.value || '50', 10);
    const description = document.getElementById('pii-description')?.value.trim();
    const testExamplesStr = document.getElementById('pii-test-examples')?.value.trim();
    const isActive = document.getElementById('pii-is-active')?.checked ?? true;

    // Walidacja
    if (!name) {
      App.showToast(I18n.t('rules.pii.name_required'), 'error');
      return;
    }
    if (!pattern) {
      App.showToast(I18n.t('rules.pii.pattern_required'), 'error');
      return;
    }
    if (!validateRegex(pattern)) {
      App.showToast(I18n.t('rules.pii.pattern_invalid'), 'error');
      return;
    }
    if (testExamplesStr && !validateJson(testExamplesStr)) {
      const errDiv = document.getElementById('pii-examples-error');
      if (errDiv) {
        errDiv.textContent = I18n.t('rules.pii.json_invalid');
        errDiv.hidden = false;
      }
      App.showToast(I18n.t('rules.pii.json_invalid'), 'error');
      return;
    }

    let testExamples = null;
    if (testExamplesStr) {
      try { testExamples = JSON.parse(testExamplesStr); } catch { testExamples = null; }
    }

    const data = {
      name,
      category,
      pattern,
      replacement,
      priority,
      description,
      test_examples: testExamples,
      is_active: isActive,
    };

    try {
      if (existingRule) {
        data.id = existingRule.id;
        await ApiClient.put('/api/pii-rules', data);
        App.showToast(I18n.t('rules.pii.updated').replace('{name}', name), 'success');
      } else {
        await ApiClient.post('/api/pii-rules', data);
        App.showToast(I18n.t('rules.pii.created').replace('{name}', name), 'success');
      }
      closeModal();
      loadRules();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  return { render, mount, unmount };
})();
