// =============================================================================
// Plik: modules/prompts/Prompts.js
// Opis: Widok zarzadzania promptami - tabela CRUD, filtry po typie i modelu,
//       badge typow, integracja z PromptEditor.
// Przyklad: ViewRouter.register('prompts', Prompts);
// =============================================================================

const Prompts = (() => {
  'use strict';

  let promptsList = [];
  let filterType = '';
  let filterModel = '';
  let abortController = null;

  // Mapa badge'ow typow - zaalokowana raz na poziomie modulu
  const typeBadgeMap = {
    system: { cls: 'prompt-type-system', label: 'System' },
    suffix: { cls: 'prompt-type-suffix', label: 'Suffix' },
    template: { cls: 'prompt-type-template', label: 'Template' },
    user: { cls: 'prompt-type-user', label: 'User' },
  };

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="card">
        <div class="card-header">
          <h3 data-i18n="prompts.title">${I18n.t('prompts.title')}</h3>
          <button class="btn btn-primary btn-sm" id="btn-add-prompt" data-i18n="common.add">+ ${I18n.t('common.add')}</button>
        </div>
        <div class="card-body" style="padding-bottom: 0;">
          <div class="prompts-filters">
            <div class="form-group" style="margin-bottom: var(--spacing-sm); min-width: 160px;">
              <select id="filter-prompt-type">
                <option value="" data-i18n="prompts.all_types">${I18n.t('prompts.all_types')}</option>
                <option value="system">System</option>
                <option value="suffix">Suffix</option>
                <option value="template">Template</option>
                <option value="user">User</option>
              </select>
            </div>
            <div class="form-group" style="margin-bottom: var(--spacing-sm); min-width: 160px;">
              <select id="filter-prompt-model">
                <option value="" data-i18n="prompts.all_models">${I18n.t('prompts.all_models')}</option>
              </select>
            </div>
          </div>
        </div>
        <div class="card-body no-padding">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th data-i18n="common.name">${I18n.t('common.name')}</th>
                  <th data-i18n="prompts.editor.type">${I18n.t('prompts.editor.type')}</th>
                  <th data-i18n="playground.model">${I18n.t('playground.model')}</th>
                  <th>Wersja</th>
                  <th data-i18n="common.status">${I18n.t('common.status')}</th>
                  <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                </tr>
              </thead>
              <tbody id="prompts-list-tbody">
                <tr>
                  <td colspan="6">
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
    abortController = new AbortController();
    const signal = abortController.signal;

    loadPrompts();

    const addBtn = document.getElementById('btn-add-prompt');
    if (addBtn) {
      addBtn.addEventListener('click', () => PromptEditor.open(null, onPromptSaved), { signal });
    }

    const typeFilter = document.getElementById('filter-prompt-type');
    if (typeFilter) {
      typeFilter.addEventListener('change', () => {
        filterType = typeFilter.value;
        renderTable();
      }, { signal });
    }

    const modelFilter = document.getElementById('filter-prompt-model');
    if (modelFilter) {
      modelFilter.addEventListener('change', () => {
        filterModel = modelFilter.value;
        renderTable();
      }, { signal });
    }

    // Delegacja zdarzen na tbody zamiast N listenerow na N elementow
    const tbody = document.getElementById('prompts-list-tbody');
    if (tbody) {
      tbody.addEventListener('click', handleTableClick, { signal });
    }
  }

  // Odmontowanie
  function unmount() {
    if (abortController) {
      abortController.abort();
      abortController = null;
    }
    promptsList = [];
    filterType = '';
    filterModel = '';
    // Usun modal edytora jesli jest otwarty
    PromptEditor.close();
  }

  // Zaladowanie promptow z API
  async function loadPrompts() {
    try {
      promptsList = await ApiClient.get('/api/prompts');
      populateModelFilter();
      renderTable();
    } catch (err) {
      console.error('Blad ladowania promptow:', err);
      promptsList = [];
      renderTable();
    }
  }

  // Wypelnienie filtra modeli unikalnymi wartosciami
  function populateModelFilter() {
    const select = document.getElementById('filter-prompt-model');
    if (!select) return;

    const models = [...new Set(
      promptsList
        .map(p => p.default_model)
        .filter(Boolean)
    )].sort();

    const current = select.value;
    select.innerHTML = `<option value="">${I18n.t('prompts.all_models')}</option>` +
      models.map(m => `<option value="${Utils.escapeAttr(m)}"${m === current ? ' selected' : ''}>${Utils.escapeHtml(m)}</option>`).join('');
  }

  // Filtrowanie listy promptow
  function getFilteredPrompts() {
    return promptsList.filter(p => {
      if (filterType && p.prompt_type !== filterType) return false;
      if (filterModel && p.default_model !== filterModel) return false;
      return true;
    });
  }

  // Renderowanie tabeli
  function renderTable() {
    const tbody = document.getElementById('prompts-list-tbody');
    if (!tbody) return;

    const filtered = getFilteredPrompts();

    if (filtered.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="6">
            <div class="empty-state">
              <div class="empty-state-icon">&#128221;</div>
              <div class="empty-state-text" data-i18n="prompts.empty">${I18n.t('prompts.empty')}</div>
              <div class="empty-state-hint" data-i18n="prompts.empty_hint">${I18n.t('prompts.empty_hint')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = filtered.map(p => {
      const typeBadge = getTypeBadge(p.prompt_type);
      const statusBadge = p.is_active
        ? `<span class="badge badge-success"><span class="status-dot status-dot-green"></span>${I18n.t('common.active')}</span>`
        : `<span class="badge badge-error"><span class="status-dot status-dot-red"></span>${I18n.t('common.inactive')}</span>`;

      return `
        <tr class="prompts-row-clickable" data-prompt-id="${p.id}">
          <td>
            <strong>${Utils.escapeHtml(p.name)}</strong>
            <div style="font-size: var(--font-size-xs); color: var(--color-text-muted);">${Utils.escapeHtml(p.prompt_id)}</div>
          </td>
          <td>${typeBadge}</td>
          <td>${Utils.escapeHtml(p.default_model || '-')}</td>
          <td>v${p.version || 1}</td>
          <td>${statusBadge}</td>
          <td>
            <div style="display: flex; gap: 4px;">
              <button class="btn btn-ghost btn-sm" data-edit-prompt="${p.id}" title="${I18n.t('common.edit')}" data-i18n-title="common.edit">&#9998;</button>
              <button class="btn btn-ghost btn-sm" data-delete-prompt="${p.id}" title="${I18n.t('common.delete')}" data-i18n-title="common.delete">&#10005;</button>
            </div>
          </td>
        </tr>
      `;
    }).join('');
  }

  // Delegowany handler klikniec w tabeli
  function handleTableClick(e) {
    const editBtn = e.target.closest('[data-edit-prompt]');
    if (editBtn) {
      const id = parseInt(editBtn.dataset.editPrompt, 10);
      const prompt = promptsList.find(p => p.id === id);
      if (prompt) PromptEditor.open(prompt, onPromptSaved);
      return;
    }

    const deleteBtn = e.target.closest('[data-delete-prompt]');
    if (deleteBtn) {
      const id = parseInt(deleteBtn.dataset.deletePrompt, 10);
      const prompt = promptsList.find(p => p.id === id);
      if (prompt) confirmDelete(prompt, deleteBtn);
      return;
    }

    // Klikniecie wiersza -> edycja
    const row = e.target.closest('.prompts-row-clickable');
    if (row && !e.target.closest('button')) {
      const id = parseInt(row.dataset.promptId, 10);
      const prompt = promptsList.find(p => p.id === id);
      if (prompt) PromptEditor.open(prompt, onPromptSaved);
    }
  }

  // Potwierdzenie usuwania
  async function confirmDelete(prompt, btn) {
    if (!confirm(I18n.t('services.delete_confirm').replace('{name}', prompt.name))) return;

    if (btn) btn.disabled = true;
    try {
      await ApiClient.delete(`/api/prompts/${prompt.id}`);
      App.showToast(I18n.t('prompts.deleted').replace('{name}', prompt.name), 'success');
      loadPrompts();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  // Callback po zapisie promptu
  function onPromptSaved() {
    loadPrompts();
  }

  // Badge typu promptu
  function getTypeBadge(type) {
    const t = typeBadgeMap[type] || { cls: 'prompt-type-user', label: type || '-' };
    return `<span class="badge prompt-type-badge ${t.cls}">${t.label}</span>`;
  }

  return { render, mount, unmount };
})();
