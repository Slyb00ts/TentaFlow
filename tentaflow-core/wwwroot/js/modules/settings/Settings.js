// =============================================================================
// Plik: modules/settings/Settings.js
// Opis: Widok ustawien globalnych routera - tabela key-value z edycja inline.
// Przyklad: ViewRouter.register('settings', Settings);
// =============================================================================

const Settings = (() => {
  'use strict';

  let settingsList = [];
  let editingKey = null;

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="card">
        <div class="card-header">
          <h3 data-i18n="settings.title">${I18n.t('settings.title')}</h3>
          <button class="btn btn-secondary btn-sm" id="btn-refresh-settings" data-i18n="settings.refresh">${I18n.t('settings.refresh')}</button>
        </div>
        <div class="card-body no-padding">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th data-i18n="settings.key">${I18n.t('settings.key')}</th>
                  <th data-i18n="settings.value">${I18n.t('settings.value')}</th>
                  <th data-i18n="settings.last_change">${I18n.t('settings.last_change')}</th>
                  <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                </tr>
              </thead>
              <tbody id="settings-tbody">
                <tr>
                  <td colspan="4">
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

      <div class="card" style="margin-top: var(--spacing-lg);">
        <div class="card-header">
          <h3 data-i18n="settings.speakers.title">${I18n.t('settings.speakers.title')}</h3>
        </div>
        <div class="card-body">
          <div class="form-group">
            <label for="setting-speaker-high" data-i18n="settings.speakers.conf_high">${I18n.t('settings.speakers.conf_high')}</label>
            <div style="display: flex; align-items: center; gap: var(--spacing-sm);">
              <input type="range" id="setting-speaker-high" min="0" max="1" step="0.01" value="0.78" style="flex: 1;">
              <span id="setting-speaker-high-val" style="min-width: 40px; text-align: right; font-size: var(--font-size-sm); color: var(--color-text-secondary);">0.78</span>
            </div>
            <div class="form-hint" data-i18n="settings.speakers.conf_high_hint">${I18n.t('settings.speakers.conf_high_hint')}</div>
          </div>
          <div class="form-group">
            <label for="setting-speaker-medium" data-i18n="settings.speakers.conf_medium">${I18n.t('settings.speakers.conf_medium')}</label>
            <div style="display: flex; align-items: center; gap: var(--spacing-sm);">
              <input type="range" id="setting-speaker-medium" min="0" max="1" step="0.01" value="0.55" style="flex: 1;">
              <span id="setting-speaker-medium-val" style="min-width: 40px; text-align: right; font-size: var(--font-size-sm); color: var(--color-text-secondary);">0.55</span>
            </div>
            <div class="form-hint" data-i18n="settings.speakers.conf_medium_hint">${I18n.t('settings.speakers.conf_medium_hint')}</div>
          </div>
          <div class="form-group">
            <label for="setting-voice-samples" data-i18n="settings.speakers.samples_required">${I18n.t('settings.speakers.samples_required')}</label>
            <input type="number" id="setting-voice-samples" min="1" max="20" value="3" style="max-width: 120px;">
            <div class="form-hint" data-i18n="settings.speakers.samples_required_hint">${I18n.t('settings.speakers.samples_required_hint')}</div>
          </div>
          <div class="form-group">
            <label for="setting-enrollment-conf" data-i18n="settings.speakers.enrollment_conf">${I18n.t('settings.speakers.enrollment_conf')}</label>
            <div style="display: flex; align-items: center; gap: var(--spacing-sm);">
              <input type="range" id="setting-enrollment-conf" min="0" max="1" step="0.01" value="0.7" style="flex: 1;">
              <span id="setting-enrollment-conf-val" style="min-width: 40px; text-align: right; font-size: var(--font-size-sm); color: var(--color-text-secondary);">0.70</span>
            </div>
            <div class="form-hint" data-i18n="settings.speakers.enrollment_conf_hint">${I18n.t('settings.speakers.enrollment_conf_hint')}</div>
          </div>
          <button class="btn btn-primary btn-sm" id="btn-save-speaker" data-i18n="settings.speakers.save">${I18n.t('settings.speakers.save')}</button>
        </div>
      </div>

      <div class="card" style="margin-top: var(--spacing-lg);">
        <div class="card-header">
          <h3 data-i18n="settings.flow.title">${I18n.t('settings.flow.title')}</h3>
        </div>
        <div class="card-body">
          <div class="form-group">
            <label style="display: flex; align-items: center; gap: var(--spacing-sm); cursor: pointer;">
              <input type="checkbox" id="setting-flow-enabled" style="width: auto;">
              <span data-i18n="settings.flow.enabled">${I18n.t('settings.flow.enabled')}</span>
            </label>
            <div class="form-hint" data-i18n="settings.flow.enabled_hint">${I18n.t('settings.flow.enabled_hint')}</div>
          </div>
          <div class="form-group">
            <label style="display: flex; align-items: center; gap: var(--spacing-sm); cursor: pointer;">
              <input type="checkbox" id="setting-flow-debug" style="width: auto;">
              <span data-i18n="settings.flow.debug">${I18n.t('settings.flow.debug')}</span>
            </label>
            <div class="form-hint" data-i18n="settings.flow.debug_hint">${I18n.t('settings.flow.debug_hint')}</div>
          </div>
          <div class="form-group">
            <label for="setting-flow-timeout" data-i18n="settings.flow.timeout">${I18n.t('settings.flow.timeout')}</label>
            <input type="number" id="setting-flow-timeout" min="1000" max="600000" step="1000" value="120000" style="max-width: 160px;">
            <div class="form-hint" data-i18n="settings.flow.timeout_hint">${I18n.t('settings.flow.timeout_hint')}</div>
          </div>
          <button class="btn btn-primary btn-sm" id="btn-save-flow-engine" data-i18n="settings.flow.save">${I18n.t('settings.flow.save')}</button>
        </div>
      </div>

      <div class="card" style="margin-top: var(--spacing-lg);">
        <div class="card-header">
          <h3 data-i18n="settings.portainer.title">${I18n.t('settings.portainer.title')}</h3>
          <button class="btn btn-secondary btn-sm" id="btn-refresh-portainer" data-i18n="settings.refresh">${I18n.t('settings.refresh')}</button>
        </div>
        <div class="card-body">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th data-i18n="common.name">${I18n.t('common.name')}</th>
                  <th data-i18n="settings.portainer.url">${I18n.t('settings.portainer.url')}</th>
                  <th data-i18n="settings.portainer.auth">${I18n.t('settings.portainer.auth')}</th>
                  <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                </tr>
              </thead>
              <tbody id="portainer-instances-tbody">
                <tr><td colspan="4"><div class="empty-state"><div class="empty-state-text" data-i18n="common.loading">${I18n.t('common.loading')}</div></div></td></tr>
              </tbody>
            </table>
          </div>
          <div style="margin-top: var(--spacing-md); padding-top: var(--spacing-md); border-top: 1px solid var(--color-border);">
            <h4 style="margin-bottom: var(--spacing-sm);" data-i18n="settings.portainer.add_title">${I18n.t('settings.portainer.add_title')}</h4>
            <div class="form-group">
              <label for="pi-name" data-i18n="settings.portainer.form.name">${I18n.t('settings.portainer.form.name')}</label>
              <input type="text" id="pi-name" placeholder="${I18n.t('settings.portainer.form.name_placeholder')}" data-i18n-placeholder="settings.portainer.form.name_placeholder">
            </div>
            <div class="form-group">
              <label for="pi-url" data-i18n="settings.portainer.url">${I18n.t('settings.portainer.url')}</label>
              <input type="text" id="pi-url" placeholder="${I18n.t('settings.portainer.form.url_placeholder')}" data-i18n-placeholder="settings.portainer.form.url_placeholder">
            </div>
            <div class="form-group">
              <label for="pi-apikey" data-i18n="settings.portainer.form.apikey">${I18n.t('settings.portainer.form.apikey')}</label>
              <input type="password" id="pi-apikey" placeholder="ptr_...">
            </div>
            <div style="margin: var(--spacing-sm) 0; text-align: center; color: var(--color-text-secondary); font-size: 0.85em;" data-i18n="settings.portainer.form.or_login">
              ${I18n.t('settings.portainer.form.or_login')}
            </div>
            <div class="form-group">
              <label for="pi-username" data-i18n="settings.portainer.form.username">${I18n.t('settings.portainer.form.username')}</label>
              <input type="text" id="pi-username" placeholder="admin">
            </div>
            <div class="form-group">
              <label for="pi-password" data-i18n="settings.portainer.form.password">${I18n.t('settings.portainer.form.password')}</label>
              <input type="password" id="pi-password" placeholder="${I18n.t('common.password')}" data-i18n-placeholder="common.password">
            </div>
            <button class="btn btn-primary btn-sm" id="btn-add-portainer" data-i18n="common.add">${I18n.t('common.add')}</button>
          </div>
        </div>
      </div>

      <div class="card" style="margin-top: var(--spacing-lg);">
        <div class="card-header">
          <h3>SSO / OIDC</h3>
          <button class="btn btn-secondary btn-sm" id="btn-refresh-sso">Odswiez</button>
        </div>
        <div class="card-body">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>Nazwa</th>
                  <th>Typ</th>
                  <th>Discovery URL</th>
                  <th>Auto-create</th>
                  <th>Status</th>
                  <th>Akcje</th>
                </tr>
              </thead>
              <tbody id="sso-providers-tbody">
                <tr><td colspan="6"><div class="empty-state"><div class="empty-state-text">Ladowanie...</div></div></td></tr>
              </tbody>
            </table>
          </div>
          <div style="margin-top: var(--spacing-md); padding-top: var(--spacing-md); border-top: 1px solid var(--color-border);">
            <h4 style="margin-bottom: var(--spacing-sm);">Dodaj SSO provider</h4>
            <div class="form-group">
              <label for="sso-name">Nazwa wyswietlana</label>
              <input type="text" id="sso-name" placeholder="np. Azure AD Firma">
            </div>
            <div class="form-group">
              <label for="sso-type">Typ providera</label>
              <select id="sso-type">
                <option value="azure_ad">Azure AD</option>
                <option value="google">Google</option>
                <option value="adfs">ADFS</option>
                <option value="authentik">Authentik</option>
                <option value="oidc">Generic OIDC</option>
              </select>
            </div>
            <div class="form-group">
              <label for="sso-client-id">Client ID</label>
              <input type="text" id="sso-client-id" placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx">
            </div>
            <div class="form-group">
              <label for="sso-client-secret">Client Secret</label>
              <input type="password" id="sso-client-secret" placeholder="Tajny klucz klienta">
            </div>
            <div class="form-group">
              <label for="sso-discovery-url">Discovery URL (issuer)</label>
              <input type="text" id="sso-discovery-url" placeholder="https://login.microsoftonline.com/{tenant}/v2.0">
              <div class="form-hint">URL bazowy providera OIDC (/.well-known/openid-configuration zostanie dodane automatycznie)</div>
            </div>
            <div class="form-group">
              <label style="display: flex; align-items: center; gap: var(--spacing-sm); cursor: pointer;">
                <input type="checkbox" id="sso-auto-create" style="width: auto;">
                <span>Automatyczne tworzenie uzytkownikow</span>
              </label>
              <div class="form-hint">Jesli wlaczone, nowi uzytkownicy SSO beda automatycznie tworzeni przy pierwszym logowaniu</div>
            </div>
            <div class="form-group">
              <label for="sso-default-group">Domyslna grupa (ID, opcjonalnie)</label>
              <input type="number" id="sso-default-group" placeholder="" style="max-width: 120px;">
            </div>
            <button class="btn btn-primary btn-sm" id="btn-add-sso">Dodaj provider</button>
          </div>
        </div>
      </div>

      <div class="card" style="margin-top: var(--spacing-lg);">
        <div class="card-header">
          <h3>OAuth Redirect URL</h3>
        </div>
        <div class="card-body">
          <div class="form-group">
            <label for="setting-oauth-redirect-url">Adres URL serwera (redirect)</label>
            <input type="text" id="setting-oauth-redirect-url" placeholder="https://localhost:8090" value="https://localhost:8090">
            <div class="form-hint">Adres URL tego serwera widoczny z przegladarki. Domyslnie localhost, zmien na publiczny adres jesli serwer jest dostepny z internetu. Uzywany jako redirect URI w OAuth/SSO flow.</div>
          </div>
          <button class="btn btn-primary btn-sm" id="btn-save-oauth-redirect">Zapisz</button>
        </div>
      </div>

      <div class="card" style="margin-top: var(--spacing-lg);">
        <div class="card-header">
          <h3 data-i18n="settings.tls.title">${I18n.t('settings.tls.title')} <span id="tls-status-icon"></span></h3>
        </div>
        <div class="card-body">
          <p style="margin-bottom: var(--spacing-md); color: var(--color-text-secondary); font-size: var(--font-size-sm);" data-i18n="settings.tls.subtitle">
            ${I18n.t('settings.tls.subtitle')}
          </p>
          <div style="display: flex; gap: var(--spacing-lg); flex-wrap: wrap;">
            <div style="flex: 1; min-width: 280px;">
              <div class="form-group">
                <label for="tls-cert-file" data-i18n="settings.tls.cert_label">${I18n.t('settings.tls.cert_label')}</label>
                <input type="file" id="tls-cert-file" accept=".pem,.crt,.key" class="form-control" style="padding: var(--spacing-xs);">
              </div>
              <div class="form-group">
                <textarea id="tls-cert-pem" rows="8" class="form-control" placeholder="-----BEGIN CERTIFICATE-----..." style="font-family: monospace; white-space: pre; resize: vertical;"></textarea>
              </div>
            </div>
            <div style="flex: 1; min-width: 280px;">
              <div class="form-group">
                <label for="tls-key-file" data-i18n="settings.tls.key_label">${I18n.t('settings.tls.key_label')}</label>
                <input type="file" id="tls-key-file" accept=".pem,.crt,.key" class="form-control" style="padding: var(--spacing-xs);">
              </div>
              <div class="form-group">
                <textarea id="tls-key-pem" rows="8" class="form-control" placeholder="-----BEGIN PRIVATE KEY-----..." style="font-family: monospace; white-space: pre; resize: vertical;"></textarea>
              </div>
            </div>
          </div>
          <div style="display: flex; gap: var(--spacing-sm); margin-top: var(--spacing-sm);">
            <button id="btn-save-tls" class="btn btn-primary btn-sm" data-i18n="settings.tls.save">${I18n.t('settings.tls.save')}</button>
            <button id="btn-provision-certs" class="btn btn-secondary btn-sm" data-i18n="settings.tls.provision">${I18n.t('settings.tls.provision')}</button>
          </div>
        </div>
      </div>

      <div class="card" style="margin-top: var(--spacing-lg);">
        <div class="card-header">
          <h3 data-i18n="settings.nvidia_title">${I18n.t('settings.nvidia_title')}</h3>
        </div>
        <div class="card-body">
          <div class="form-group">
            <label for="setting-ngc-api-key" data-i18n="settings.ngc_api_key">${I18n.t('settings.ngc_api_key')}</label>
            <div style="display: flex; gap: var(--spacing-sm); align-items: center;">
              <input type="password" id="setting-ngc-api-key" placeholder="nvapi-..." style="flex: 1;">
              <span id="ngc-status-badge" style="font-size: var(--font-size-sm); padding: 2px 8px; border-radius: 4px;"></span>
            </div>
            <div class="form-hint" data-i18n="settings.ngc_api_key_hint">${I18n.t('settings.ngc_api_key_hint')}</div>
          </div>
          <div style="display: flex; gap: var(--spacing-sm);">
            <button class="btn btn-primary btn-sm" id="btn-save-ngc-key" data-i18n="common.save">${I18n.t('common.save')}</button>
            <button class="btn btn-secondary btn-sm" id="btn-test-ngc" data-i18n="settings.ngc_test">${I18n.t('settings.ngc_test')}</button>
          </div>
        </div>
      </div>
    `;
  }

  // Montowanie
  function mount() {
    loadSettings();

    document.getElementById('btn-refresh-settings')?.addEventListener('click', loadSettings);

    // Slidery rozpoznawania mowcow
    initSlider('setting-speaker-high', 'setting-speaker-high-val');
    initSlider('setting-speaker-medium', 'setting-speaker-medium-val');
    initSlider('setting-enrollment-conf', 'setting-enrollment-conf-val');

    // Zapis ustawien mowcow
    document.getElementById('btn-save-speaker')?.addEventListener('click', saveSpeakerSettings);

    // Zapis ustawien flow engine
    document.getElementById('btn-save-flow-engine')?.addEventListener('click', saveFlowEngineSettings);

    // OAuth redirect URL
    document.getElementById('btn-save-oauth-redirect')?.addEventListener('click', saveOauthRedirectUrl);

    // SSO providers
    document.getElementById('btn-refresh-sso')?.addEventListener('click', loadSsoProviders);
    document.getElementById('btn-add-sso')?.addEventListener('click', addSsoProvider);
    loadSsoProviders();

    // Portainer instances
    document.getElementById('btn-refresh-portainer')?.addEventListener('click', loadPortainerInstances);
    document.getElementById('btn-add-portainer')?.addEventListener('click', addPortainerInstance);
    loadPortainerInstances();

    // Certyfikaty TLS - wczytywanie plikow do textarea
    document.getElementById('tls-cert-file')?.addEventListener('change', (e) => {
      const file = e.target.files[0];
      if (!file) return;
      const reader = new FileReader();
      reader.onload = (ev) => {
        document.getElementById('tls-cert-pem').value = ev.target.result;
      };
      reader.readAsText(file);
    });

    document.getElementById('tls-key-file')?.addEventListener('change', (e) => {
      const file = e.target.files[0];
      if (!file) return;
      const reader = new FileReader();
      reader.onload = (ev) => {
        document.getElementById('tls-key-pem').value = ev.target.result;
      };
      reader.readAsText(file);
    });

    // Zapis certyfikatow TLS
    document.getElementById('btn-save-tls')?.addEventListener('click', saveTlsCerts);

    // Dystrybucja certyfikatow do agentow
    document.getElementById('btn-provision-certs')?.addEventListener('click', provisionCerts);

    // NGC API Key
    document.getElementById('btn-save-ngc-key')?.addEventListener('click', saveNgcApiKey);
    document.getElementById('btn-test-ngc')?.addEventListener('click', testNgcConnection);

    // Zaladuj obecne certyfikaty
    loadTlsCerts();
  }

  // Odmontowanie
  function unmount() {
    settingsList = [];
    editingKey = null;
  }

  // Zaladowanie ustawien z API
  async function loadSettings() {
    try {
      settingsList = await ApiClient.get('/api/settings');
      renderTable();
      applySettingsToForms();
    } catch (err) {
      console.error('Blad ladowania ustawien:', err);
      settingsList = [];
      renderTable();
    }
  }

  // Renderowanie tabeli
  function renderTable() {
    const tbody = document.getElementById('settings-tbody');
    if (!tbody) return;

    if (settingsList.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="4">
            <div class="empty-state">
              <div class="empty-state-icon">&#9881;</div>
              <div class="empty-state-text" data-i18n="common.no_data">${I18n.t('common.no_data')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = settingsList.map(s => {
      const isEditing = editingKey === s.key;
      const isSensitive = s.key.includes('secret') || s.key.includes('password');
      const displayValue = isSensitive ? '********' : Utils.escapeHtml(s.value);

      if (isEditing) {
        return `
          <tr>
            <td><code>${Utils.escapeHtml(s.key)}</code></td>
            <td>
              <div class="inline-edit">
                <input type="text" id="edit-value-${s.key}" value="${Utils.escapeAttr(s.value)}">
                <button class="btn btn-primary btn-sm" data-save-key="${s.key}" data-i18n="common.save">${I18n.t('common.save')}</button>
                <button class="btn btn-ghost btn-sm" data-cancel-key="${s.key}" data-i18n="common.cancel">${I18n.t('common.cancel')}</button>
              </div>
            </td>
            <td>${Utils.formatDate(s.updated_at)}</td>
            <td></td>
          </tr>
        `;
      }

      return `
        <tr>
          <td><code>${Utils.escapeHtml(s.key)}</code></td>
          <td>${displayValue}</td>
          <td>${Utils.formatDate(s.updated_at)}</td>
          <td>
            <button class="btn btn-ghost btn-sm" data-edit-key="${s.key}" title="${I18n.t('common.edit')}" data-i18n-title="common.edit">&#9998;</button>
          </td>
        </tr>
      `;
    }).join('');

    // Podepnij zdarzenia
    tbody.querySelectorAll('[data-edit-key]').forEach(btn => {
      btn.addEventListener('click', () => {
        editingKey = btn.dataset.editKey;
        renderTable();
      });
    });

    tbody.querySelectorAll('[data-cancel-key]').forEach(btn => {
      btn.addEventListener('click', () => {
        editingKey = null;
        renderTable();
      });
    });

    tbody.querySelectorAll('[data-save-key]').forEach(btn => {
      btn.addEventListener('click', () => {
        const key = btn.dataset.saveKey;
        const input = document.getElementById(`edit-value-${key}`);
        if (input) saveSetting(key, input.value);
      });
    });
  }

  // Zapis ustawienia
  async function saveSetting(key, value) {
    try {
      await ApiClient.put('/api/settings', { key, value });
      editingKey = null;
      App.showToast(I18n.t('settings.save_success').replace('{key}', key), 'success');
      loadSettings();
    } catch (err) {
      App.showToast(I18n.t('settings.save_error').replace('{error}', err.message), 'error');
    }
  }

  // Inicjalizacja slidera z wyswietlaniem wartosci
  function initSlider(sliderId, valueId) {
    const slider = document.getElementById(sliderId);
    const valueEl = document.getElementById(valueId);
    if (slider && valueEl) {
      slider.addEventListener('input', () => {
        valueEl.textContent = parseFloat(slider.value).toFixed(2);
      });
    }
  }

  // Wypelnienie pol formularzy na podstawie zaladowanych ustawien
  function applySettingsToForms() {
    const map = {};
    for (const s of settingsList) {
      map[s.key] = s.value;
    }

    // Rozpoznawanie mowcow
    setSliderValue('setting-speaker-high', 'setting-speaker-high-val', map.speaker_confidence_high, 0.78);
    setSliderValue('setting-speaker-medium', 'setting-speaker-medium-val', map.speaker_confidence_medium, 0.55);
    setSliderValue('setting-enrollment-conf', 'setting-enrollment-conf-val', map.speaker_enrollment_min_confidence, 0.7);
    setInputValue('setting-voice-samples', map.speaker_voice_samples_required, 3);

    // Flow engine
    setCheckboxValue('setting-flow-enabled', map.flow_engine_enabled);
    setCheckboxValue('setting-flow-debug', map.flow_debug_mode);
    setInputValue('setting-flow-timeout', map.flow_default_timeout_ms, 120000);

    // OAuth redirect URL
    setInputValue('setting-oauth-redirect-url', map.oauth_redirect_base_url, 'https://localhost:8090');

    // NGC API Key — pokaz status
    updateNgcBadge(map.ngc_api_key);
  }

  // Pomocniki ustawiania wartosci
  function setSliderValue(sliderId, valueId, val, defaultVal) {
    const slider = document.getElementById(sliderId);
    const valueEl = document.getElementById(valueId);
    const v = val != null ? parseFloat(val) : defaultVal;
    if (slider) slider.value = v;
    if (valueEl) valueEl.textContent = parseFloat(v).toFixed(2);
  }

  function setInputValue(inputId, val, defaultVal) {
    const input = document.getElementById(inputId);
    if (input) input.value = val != null ? val : defaultVal;
  }

  function setCheckboxValue(checkboxId, val) {
    const cb = document.getElementById(checkboxId);
    if (cb) cb.checked = val === 'true' || val === true || val === '1';
  }

  // Zapis ustawien rozpoznawania mowcow
  async function saveSpeakerSettings() {
    const settings = [
      { key: 'speaker_confidence_high', value: document.getElementById('setting-speaker-high')?.value || '0.78' },
      { key: 'speaker_confidence_medium', value: document.getElementById('setting-speaker-medium')?.value || '0.55' },
      { key: 'speaker_voice_samples_required', value: document.getElementById('setting-voice-samples')?.value || '3' },
      { key: 'speaker_enrollment_min_confidence', value: document.getElementById('setting-enrollment-conf')?.value || '0.7' },
    ];

    try {
      for (const s of settings) {
        await ApiClient.put('/api/settings', s);
      }
      App.showToast(I18n.t('settings.speakers.save_success'), 'success');
      loadSettings();
    } catch (err) {
      App.showToast(I18n.t('settings.save_error').replace('{error}', err.message), 'error');
    }
  }

  // Zapis ustawien flow engine
  async function saveFlowEngineSettings() {
    const settings = [
      { key: 'flow_engine_enabled', value: String(document.getElementById('setting-flow-enabled')?.checked || false) },
      { key: 'flow_debug_mode', value: String(document.getElementById('setting-flow-debug')?.checked || false) },
      { key: 'flow_default_timeout_ms', value: document.getElementById('setting-flow-timeout')?.value || '120000' },
    ];

    try {
      for (const s of settings) {
        await ApiClient.put('/api/settings', s);
      }
      App.showToast(I18n.t('settings.flow.save_success'), 'success');
      loadSettings();
    } catch (err) {
      App.showToast(I18n.t('settings.save_error').replace('{error}', err.message), 'error');
    }
  }

  // Zapis OAuth redirect URL
  async function saveOauthRedirectUrl() {
    const value = document.getElementById('setting-oauth-redirect-url')?.value?.trim();
    if (!value) {
      App.showToast('Podaj adres URL', 'error');
      return;
    }
    if (!value.startsWith('http://') && !value.startsWith('https://')) {
      App.showToast('URL musi zaczynac sie od http:// lub https://', 'error');
      return;
    }
    try {
      await ApiClient.put('/api/settings', { key: 'oauth_redirect_base_url', value });
      App.showToast('OAuth Redirect URL zapisany', 'success');
      loadSettings();
    } catch (err) {
      App.showToast(`Blad zapisu: ${err.message}`, 'error');
    }
  }

  // ==========================================================================
  // SSO Providers
  // ==========================================================================

  // Zaladowanie providerow SSO z API
  async function loadSsoProviders() {
    try {
      const providers = await ApiClient.get('/api/sso/providers');
      renderSsoProviders(providers || []);
    } catch (err) {
      console.error('Blad ladowania SSO providerow:', err);
      renderSsoProviders([]);
    }
  }

  // Renderowanie tabeli SSO providerow
  function renderSsoProviders(providers) {
    const tbody = document.getElementById('sso-providers-tbody');
    if (!tbody) return;

    if (providers.length === 0) {
      tbody.innerHTML = `<tr><td colspan="6"><div class="empty-state"><div class="empty-state-icon">&#128274;</div><div class="empty-state-text">Brak skonfigurowanych providerow SSO</div></div></td></tr>`;
      return;
    }

    const typeLabels = {
      'azure_ad': 'Azure AD',
      'google': 'Google',
      'adfs': 'ADFS',
      'authentik': 'Authentik',
      'oidc': 'Generic OIDC'
    };

    tbody.innerHTML = providers.map(p => `
      <tr>
        <td>${Utils.escapeHtml(p.name)}</td>
        <td>${typeLabels[p.provider_type] || Utils.escapeHtml(p.provider_type)}</td>
        <td><code style="font-size: 0.85em;">${Utils.escapeHtml(p.discovery_url)}</code></td>
        <td>${p.auto_create_users ? 'Tak' : 'Nie'}</td>
        <td><span class="badge ${p.enabled ? 'badge-success' : 'badge-danger'}">${p.enabled ? 'Aktywny' : 'Wylaczony'}</span></td>
        <td>
          <div style="display:flex;gap:var(--spacing-xs);">
            <button class="btn btn-ghost btn-sm" data-sso-test="${p.id}" title="Testuj polaczenie">&#128268;</button>
            <button class="btn btn-ghost btn-sm btn-danger-text" data-sso-delete="${p.id}" title="Usun">&#10005;</button>
          </div>
        </td>
      </tr>
    `).join('');

    // Test polaczenia
    tbody.querySelectorAll('[data-sso-test]').forEach(btn => {
      btn.addEventListener('click', () => testSsoProvider(parseInt(btn.dataset.ssoTest)));
    });

    // Usuniecie providera
    tbody.querySelectorAll('[data-sso-delete]').forEach(btn => {
      btn.addEventListener('click', () => deleteSsoProvider(parseInt(btn.dataset.ssoDelete)));
    });
  }

  // Dodanie nowego SSO providera
  async function addSsoProvider() {
    const name = document.getElementById('sso-name')?.value?.trim();
    const providerType = document.getElementById('sso-type')?.value;
    const clientId = document.getElementById('sso-client-id')?.value?.trim();
    const clientSecret = document.getElementById('sso-client-secret')?.value?.trim();
    const discoveryUrl = document.getElementById('sso-discovery-url')?.value?.trim();
    const autoCreate = document.getElementById('sso-auto-create')?.checked || false;
    const defaultGroupStr = document.getElementById('sso-default-group')?.value?.trim();
    const defaultGroupId = defaultGroupStr ? parseInt(defaultGroupStr) : null;

    if (!name || !clientId || !clientSecret || !discoveryUrl) {
      App.showToast('Wypelnij wszystkie wymagane pola (nazwa, client ID, client secret, discovery URL)', 'error');
      return;
    }

    try {
      await ApiClient.post('/api/sso/providers', {
        name,
        provider_type: providerType,
        client_id: clientId,
        client_secret: clientSecret,
        discovery_url: discoveryUrl,
        auto_create_users: autoCreate,
        default_group_id: defaultGroupId
      });
      App.showToast('SSO provider dodany pomyslnie', 'success');
      // Wyczysc formularz
      ['sso-name', 'sso-client-id', 'sso-client-secret', 'sso-discovery-url', 'sso-default-group'].forEach(id => {
        const el = document.getElementById(id);
        if (el) el.value = '';
      });
      const autoCreateCb = document.getElementById('sso-auto-create');
      if (autoCreateCb) autoCreateCb.checked = false;
      loadSsoProviders();
    } catch (err) {
      App.showToast(`Blad dodawania SSO providera: ${err.message}`, 'error');
    }
  }

  // Usuniecie SSO providera
  async function deleteSsoProvider(id) {
    if (!confirm('Czy na pewno chcesz usunac tego SSO providera?')) return;
    try {
      await ApiClient.delete(`/api/sso/providers/${id}`);
      App.showToast('SSO provider usuniety', 'success');
      loadSsoProviders();
    } catch (err) {
      App.showToast(`Blad: ${err.message}`, 'error');
    }
  }

  // Test SSO providera — proba discovery
  async function testSsoProvider(id) {
    try {
      const result = await ApiClient.get(`/api/sso/login/${id}`);
      if (result && result.auth_url) {
        App.showToast('Provider OIDC dziala poprawnie. Discovery URL jest osiagalny.', 'success');
      } else {
        App.showToast('Brak auth_url w odpowiedzi — sprawdz konfiguracje', 'warning');
      }
    } catch (err) {
      App.showToast(`Blad testowania SSO: ${err.message}`, 'error');
    }
  }

  // Zaladowanie instancji Portainer
  async function loadPortainerInstances() {
    try {
      const instances = await ApiClient.get('/api/portainer-instances');
      renderPortainerInstances(instances || []);
    } catch (err) {
      console.error('Blad ladowania instancji Portainer:', err);
      renderPortainerInstances([]);
    }
  }

  // Renderowanie tabeli instancji
  function renderPortainerInstances(instances) {
    const tbody = document.getElementById('portainer-instances-tbody');
    if (!tbody) return;

    if (instances.length === 0) {
      tbody.innerHTML = `<tr><td colspan="4"><div class="empty-state"><div class="empty-state-icon">&#9881;</div><div class="empty-state-text" data-i18n="settings.portainer.empty">${I18n.t('settings.portainer.empty')}</div></div></td></tr>`;
      return;
    }

    tbody.innerHTML = instances.map(inst => {
      const authInfo = inst.api_key && inst.api_key !== '***'
        ? Utils.escapeHtml(inst.api_key)
        : inst.username
          ? 'Login: ' + Utils.escapeHtml(inst.username)
          : I18n.t('settings.portainer.auth_none');
      return `
      <tr>
        <td>${Utils.escapeHtml(inst.name)}</td>
        <td><code>${Utils.escapeHtml(inst.url)}</code></td>
        <td><code>${authInfo}</code></td>
        <td>
          <div style="display:flex;gap:var(--spacing-xs);">
            <button class="btn btn-ghost btn-sm" data-test-instance="${inst.id}" title="${I18n.t('settings.portainer.test')}" data-i18n-title="settings.portainer.test">&#128268;</button>
            <button class="btn btn-ghost btn-sm btn-danger-text" data-delete-instance="${inst.id}" title="${I18n.t('common.delete')}" data-i18n-title="common.delete">&#10005;</button>
          </div>
          <span class="pi-test-result" data-result-for="${inst.id}" style="font-size:var(--font-size-sm);"></span>
        </td>
      </tr>
    `; }).join('');

    tbody.querySelectorAll('[data-test-instance]').forEach(btn => {
      btn.addEventListener('click', () => testPortainerInstance(parseInt(btn.dataset.testInstance)));
    });

    tbody.querySelectorAll('[data-delete-instance]').forEach(btn => {
      btn.addEventListener('click', () => deletePortainerInstance(parseInt(btn.dataset.deleteInstance)));
    });
  }

  // Dodanie instancji Portainer
  async function addPortainerInstance() {
    const name = document.getElementById('pi-name')?.value?.trim();
    const url = document.getElementById('pi-url')?.value?.trim();
    const apiKey = document.getElementById('pi-apikey')?.value?.trim();
    const username = document.getElementById('pi-username')?.value?.trim();
    const password = document.getElementById('pi-password')?.value?.trim();

    if (!name || !url) {
      App.showToast(I18n.t('settings.portainer.add_error_name_url'), 'error');
      return;
    }
    if (!apiKey && (!username || !password)) {
      App.showToast(I18n.t('settings.portainer.add_error_auth'), 'error');
      return;
    }

    try {
      await ApiClient.post('/api/portainer-instances', {
        name,
        url,
        api_key: apiKey || '',
        username: username || '',
        password: password || ''
      });
      App.showToast(I18n.t('settings.portainer.add_success'), 'success');
      // Wyczysc formularz
      ['pi-name', 'pi-url', 'pi-apikey', 'pi-username', 'pi-password'].forEach(id => {
        const el = document.getElementById(id);
        if (el) el.value = '';
      });
      loadPortainerInstances();
    } catch (err) {
      App.showToast(I18n.t('settings.portainer.add_error').replace('{error}', err.message), 'error');
    }
  }

  // Usuniecie instancji Portainer
  async function deletePortainerInstance(id) {
    if (!confirm(I18n.t('settings.portainer.delete_confirm'))) return;
    try {
      await ApiClient.delete(`/api/portainer-instances/${id}`);
      App.showToast(I18n.t('settings.portainer.delete_success'), 'success');
      loadPortainerInstances();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  // Test polaczenia z instancja Portainer
  async function testPortainerInstance(id) {
    const resultEl = document.querySelector(`[data-result-for="${id}"]`);
    if (resultEl) {
      resultEl.textContent = I18n.t('settings.portainer.testing');
      resultEl.style.color = 'var(--color-text-secondary)';
    }
    try {
      const data = await ApiClient.get(`/api/portainer/instances/${id}/status`);
      if (resultEl) {
        if (data.connected) {
          resultEl.textContent = I18n.t('settings.portainer.test_success').replace('{count}', data.endpoint_count);
          resultEl.style.color = 'var(--color-success)';
        } else {
          resultEl.textContent = I18n.t('settings.portainer.test_error').replace('{error}', data.error || 'Unknown');
          resultEl.style.color = 'var(--color-danger)';
        }
      }
    } catch (err) {
      if (resultEl) {
        resultEl.textContent = I18n.t('settings.portainer.test_error').replace('{error}', err.message);
        resultEl.style.color = 'var(--color-danger)';
      }
    }
  }

  // Zaladowanie obecnych certyfikatow TLS z ustawien
  async function loadTlsCerts() {
    try {
      const settings = await ApiClient.get('/api/settings');
      const map = {};
      for (const s of settings) {
        map[s.key] = s.value;
      }

      const certArea = document.getElementById('tls-cert-pem');
      const keyArea = document.getElementById('tls-key-pem');
      const statusIcon = document.getElementById('tls-status-icon');

      if (certArea && map.tls_cert_pem) certArea.value = map.tls_cert_pem;
      if (keyArea && map.tls_key_pem) keyArea.value = map.tls_key_pem;

      // Pokaz ikone statusu
      if (statusIcon) {
        if (map.tls_cert_pem && map.tls_key_pem) {
          statusIcon.textContent = '\u2713';
          statusIcon.style.color = 'var(--color-success)';
          statusIcon.title = I18n.t('settings.tls.status_loaded');
        } else {
          statusIcon.textContent = '';
        }
      }
    } catch (err) {
      console.error('Blad ladowania certyfikatow TLS:', err);
    }
  }

  // Zapis certyfikatow TLS
  async function saveTlsCerts() {
    const certValue = document.getElementById('tls-cert-pem')?.value?.trim();
    const keyValue = document.getElementById('tls-key-pem')?.value?.trim();

    if (!certValue || !keyValue) {
      App.showToast(I18n.t('settings.tls.save_error_required'), 'error');
      return;
    }

    try {
      await ApiClient.put('/api/settings', { key: 'tls_cert_pem', value: certValue });
      await ApiClient.put('/api/settings', { key: 'tls_key_pem', value: keyValue });
      App.showToast(I18n.t('settings.tls.save_success'), 'success');
      loadSettings();
      loadTlsCerts();
    } catch (err) {
      App.showToast(I18n.t('settings.tls.save_error').replace('{error}', err.message), 'error');
    }
  }

  // Dystrybucja certyfikatow do wszystkich agentow
  async function provisionCerts() {
    try {
      const resp = await ApiClient.post('/api/agents/provision-certs');
      const results = resp?.results || [];
      if (results.length === 0) {
        App.showToast(I18n.t('settings.tls.provision_no_agents'), 'warning');
        return;
      }
      const ok = results.filter(r => r.success);
      const fail = results.filter(r => !r.success);
      if (fail.length === 0) {
        App.showToast(I18n.t('settings.tls.provision_success').replace('{count}', ok.length), 'success');
      } else {
        const errors = fail.map(r => `${r.agent_id}: ${r.error || 'unknown error'}`).join('\n');
        App.showToast(I18n.t('settings.tls.provision_warning').replace('{ok}', ok.length).replace('{fail}', fail.length) + `\n${errors}`, fail.length === results.length ? 'error' : 'warning');
      }
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  // Status badge NGC
  function updateNgcBadge(val) {
    const badge = document.getElementById('ngc-status-badge');
    if (!badge) return;
    if (val && val !== '***') {
      badge.textContent = I18n.t('settings.ngc_configured');
      badge.style.background = 'var(--color-success, #22c55e)';
      badge.style.color = '#fff';
    } else {
      badge.textContent = I18n.t('settings.ngc_not_configured');
      badge.style.background = 'var(--color-border, #555)';
      badge.style.color = 'var(--color-text-secondary, #aaa)';
    }
  }

  // Zapis NGC API Key
  async function saveNgcApiKey() {
    const input = document.getElementById('setting-ngc-api-key');
    const value = input?.value?.trim();
    if (!value) return;
    try {
      await ApiClient.put('/api/settings', { key: 'ngc_api_key', value });
      input.value = '';
      App.showToast(I18n.t('settings.save_success').replace('{key}', 'ngc_api_key'), 'success');
      loadSettings();
    } catch (err) {
      App.showToast(I18n.t('settings.save_error').replace('{error}', err.message), 'error');
    }
  }

  // Test polaczenia NGC
  async function testNgcConnection() {
    const btn = document.getElementById('btn-test-ngc');
    if (btn) btn.disabled = true;
    try {
      await ApiClient.get('/api/nim/catalog');
      App.showToast(I18n.t('settings.ngc_test_success'), 'success');
    } catch (err) {
      App.showToast(I18n.t('settings.ngc_test_failed') + ': ' + err.message, 'error');
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  return { render, mount, unmount };
})();
