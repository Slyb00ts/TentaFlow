// =============================================================================
// Plik: modules/apikeys/ApiKeys.js
// Opis: Zarzadzanie kluczami API - lista, generowanie, dezaktywacja.
// Przyklad: ViewRouter.register('apikeys', ApiKeys);
// =============================================================================

const ApiKeys = (() => {
  'use strict';

  let keysList = [];

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="card">
        <div class="card-header">
          <h3 data-i18n="apikeys.title">${I18n.t('apikeys.title')}</h3>
          <button class="btn btn-primary btn-sm" id="btn-generate-key" data-i18n="apikeys.generate">+ ${I18n.t('apikeys.generate')}</button>
        </div>
        <div class="card-body no-padding">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th data-i18n="apikeys.prefix">${I18n.t('apikeys.prefix')}</th>
                  <th data-i18n="common.name">${I18n.t('common.name')}</th>
                  <th data-i18n="apikeys.rate_limit">${I18n.t('apikeys.rate_limit')}</th>
                  <th data-i18n="common.status">${I18n.t('common.status')}</th>
                  <th data-i18n="common.created_at">${I18n.t('common.created_at')}</th>
                  <th data-i18n="apikeys.last_used">${I18n.t('apikeys.last_used')}</th>
                  <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                </tr>
              </thead>
              <tbody id="apikeys-tbody">
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

      <!-- Modal generowania klucza -->
      <div class="modal-overlay" id="generate-key-modal">
        <div class="modal">
          <div class="modal-header">
            <h3 data-i18n="apikeys.generate_title">${I18n.t('apikeys.generate_title')}</h3>
            <button class="modal-close" id="gen-modal-close">&times;</button>
          </div>
          <div class="modal-body">
            <div class="form-group">
              <label for="key-name" data-i18n="apikeys.key_name">${I18n.t('apikeys.key_name')}</label>
              <input type="text" id="key-name" placeholder="${I18n.t('apikeys.key_name_placeholder')}" data-i18n-placeholder="apikeys.key_name_placeholder" required>
            </div>
            <div class="form-group">
              <label for="key-rate-limit" data-i18n="apikeys.rate_limit">${I18n.t('apikeys.rate_limit')}</label>
              <input type="number" id="key-rate-limit" value="100" min="1" max="10000">
            </div>
            <div id="gen-key-error" class="form-error" hidden></div>
            <div id="gen-key-result" hidden>
              <div class="form-group">
                <label data-i18n="apikeys.key_result_label">${I18n.t('apikeys.key_result_label')}</label>
                <div class="inline-edit">
                  <input type="text" id="gen-key-value" readonly>
                  <button class="btn btn-secondary btn-sm" id="btn-copy-key" data-i18n="apikeys.copy">${I18n.t('apikeys.copy')}</button>
                </div>
              </div>
            </div>
          </div>
          <div class="modal-footer">
            <button class="btn btn-secondary" id="gen-modal-cancel" data-i18n="apikeys.close">${I18n.t('apikeys.close')}</button>
            <button class="btn btn-primary" id="gen-modal-submit" data-i18n="apikeys.generate">${I18n.t('apikeys.generate')}</button>
          </div>
        </div>
      </div>
    `;
  }

  // Montowanie
  function mount() {
    loadKeys();

    document.getElementById('btn-generate-key')?.addEventListener('click', openGenerateModal);
    document.getElementById('gen-modal-close')?.addEventListener('click', closeGenerateModal);
    document.getElementById('gen-modal-cancel')?.addEventListener('click', closeGenerateModal);
    document.getElementById('gen-modal-submit')?.addEventListener('click', handleGenerate);
    document.getElementById('btn-copy-key')?.addEventListener('click', copyKey);
  }

  // Odmontowanie
  function unmount() {
    keysList = [];
  }

  // Zaladowanie kluczy z API
  async function loadKeys() {
    try {
      keysList = await ApiClient.get('/api/apikeys');
      renderTable();
    } catch (err) {
      console.error('Blad ladowania kluczy API:', err);
      keysList = [];
      renderTable();
    }
  }

  // Renderowanie tabeli
  function renderTable() {
    const tbody = document.getElementById('apikeys-tbody');
    if (!tbody) return;

    if (keysList.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="7">
            <div class="empty-state">
              <div class="empty-state-icon">&#128273;</div>
              <div class="empty-state-text" data-i18n="apikeys.empty">${I18n.t('apikeys.empty')}</div>
              <div class="empty-state-hint" data-i18n="apikeys.empty_hint">${I18n.t('apikeys.empty_hint')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = keysList.map(k => `
      <tr>
        <td><code>${Utils.escapeHtml(k.key_prefix)}...</code></td>
        <td>${Utils.escapeHtml(k.name)}</td>
        <td>${k.rate_limit_rps}</td>
        <td>
          <span class="badge badge-${k.is_active ? 'success' : 'error'}">
            ${k.is_active ? I18n.t('common.active') : I18n.t('common.inactive')}
          </span>
        </td>
        <td>${Utils.formatDate(k.created_at)}</td>
        <td>${k.last_used_at ? Utils.formatDate(k.last_used_at) : '-'}</td>
        <td>
          ${k.is_active
            ? `<button class="btn btn-ghost btn-sm" data-deactivate="${k.id}" title="${I18n.t('apikeys.deactivate')}" data-i18n-title="apikeys.deactivate">&#10005;</button>`
            : `<span class="badge badge-error">${I18n.t('apikeys.disabled')}</span>`
          }
        </td>
      </tr>
    `).join('');

    // Podepnij dezaktywacje
    tbody.querySelectorAll('[data-deactivate]').forEach(btn => {
      btn.addEventListener('click', () => {
        const id = parseInt(btn.dataset.deactivate, 10);
        handleDeactivate(id);
      });
    });
  }

  // Dezaktywacja klucza
  async function handleDeactivate(id) {
    const key = keysList.find(k => k.id === id);
    if (!key) return;
    if (!confirm(I18n.t('apikeys.deactivate_confirm').replace('{name}', key.name).replace('{prefix}', key.key_prefix))) return;

    try {
      await ApiClient.delete(`/api/apikeys/${id}`);
      App.showToast(I18n.t('apikeys.deactivate_success').replace('{name}', key.name), 'success');
      loadKeys();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  // Otwarcie modala generowania
  function openGenerateModal() {
    const modal = document.getElementById('generate-key-modal');
    if (modal) modal.classList.add('active');

    // Reset formularza
    const nameInput = document.getElementById('key-name');
    const rateInput = document.getElementById('key-rate-limit');
    const resultEl = document.getElementById('gen-key-result');
    const errorEl = document.getElementById('gen-key-error');

    if (nameInput) nameInput.value = '';
    if (rateInput) rateInput.value = '100';
    if (resultEl) resultEl.hidden = true;
    if (errorEl) errorEl.hidden = true;
  }

  // Zamkniecie modala
  function closeGenerateModal() {
    const modal = document.getElementById('generate-key-modal');
    if (modal) modal.classList.remove('active');
    loadKeys();
  }

  // Generowanie klucza
  async function handleGenerate() {
    const name = document.getElementById('key-name').value.trim();
    const rateLimit = parseInt(document.getElementById('key-rate-limit').value, 10) || 100;
    const errorEl = document.getElementById('gen-key-error');
    const resultEl = document.getElementById('gen-key-result');

    if (!name) {
      if (errorEl) {
        errorEl.textContent = I18n.t('apikeys.key_name_required');
        errorEl.hidden = false;
      }
      return;
    }

    try {
      const data = await ApiClient.post('/api/apikeys', { name, rate_limit_rps: rateLimit });

      // Pokaz wygenerowany klucz
      if (resultEl) {
        resultEl.hidden = false;
        const keyInput = document.getElementById('gen-key-value');
        if (keyInput) keyInput.value = data.key || data.api_key || '';
      }
      if (errorEl) errorEl.hidden = true;

      // Ukryj przycisk generowania
      const submitBtn = document.getElementById('gen-modal-submit');
      if (submitBtn) submitBtn.hidden = true;

      App.showToast(I18n.t('apikeys.generate_success').replace('{name}', name), 'success');
    } catch (err) {
      if (errorEl) {
        errorEl.textContent = err.message || I18n.t('apikeys.generate_error');
        errorEl.hidden = false;
      }
    }
  }

  // Kopiowanie klucza do schowka
  async function copyKey() {
    const keyInput = document.getElementById('gen-key-value');
    if (!keyInput) return;

    try {
      await navigator.clipboard.writeText(keyInput.value);
      App.showToast(I18n.t('apikeys.copied'), 'info');
    } catch {
      keyInput.select();
      document.execCommand('copy');
      App.showToast(I18n.t('apikeys.copied'), 'info');
    }
  }

  return { render, mount, unmount };
})();
