// =============================================================================
// Plik: modules/addons/Addons.js
// Opis: Widok zarzadzania addonami (aplikacjami) — grid kart, szczegoly,
//       instalacja ZIP, ustawienia z config schema, granularne uprawnienia,
//       logi, narzedzia, enable/disable.
// Przyklad: ViewRouter.register('addons', Addons);
// =============================================================================

const Addons = (() => {
  'use strict';

  let addons = [];
  let selectedAddon = null;
  let activeTab = 'settings';
  let abortController = null;
  let auditLogs = [];
  let auditOffset = 0;
  const AUDIT_PAGE_SIZE = 20;

  // Cache danych panelu detali — unika wielokrotnych requestow
  let cachedUiData = null;
  let cachedPermissionsData = null;

  // Renderowanie glownego HTML
  function render() {
    return `
      <div class="card">
        <div class="card-header">
          <h3 data-i18n="addons.title">${I18n.t('addons.title') || 'Aplikacje'}</h3>
          <button class="btn btn-primary btn-sm" id="btn-install-addon">
            + ${I18n.t('addons.install') || 'Zainstaluj addon'}
          </button>
        </div>
        <div class="card-body">
          <div id="addons-grid" class="addons-grid">
            <div class="empty-state">
              <div class="empty-state-text">${I18n.t('common.loading') || 'Ladowanie...'}</div>
            </div>
          </div>
        </div>
      </div>
      <div id="addon-detail-panel" class="addon-detail-panel" hidden></div>
      <input type="file" id="addon-file-input" accept=".zip" hidden>
    `;
  }

  // Montowanie — podepnij zdarzenia, zaladuj dane
  function mount() {
    abortController = new AbortController();
    const signal = abortController.signal;

    loadAddons();

    const installBtn = document.getElementById('btn-install-addon');
    if (installBtn) {
      installBtn.addEventListener('click', () => {
        document.getElementById('addon-file-input').click();
      }, { signal });
    }

    const fileInput = document.getElementById('addon-file-input');
    if (fileInput) {
      fileInput.addEventListener('change', handleFileUpload, { signal });
    }

    const grid = document.getElementById('addons-grid');
    if (grid) {
      grid.addEventListener('click', handleGridClick, { signal });
    }

    const detailPanel = document.getElementById('addon-detail-panel');
    if (detailPanel) {
      detailPanel.addEventListener('click', handleDetailClick, { signal });
      detailPanel.addEventListener('change', handleDetailChange, { signal });
      detailPanel.addEventListener('submit', handleDetailSubmit, { signal });
    }
  }

  // Odmontowanie
  function unmount() {
    if (abortController) {
      abortController.abort();
      abortController = null;
    }
    addons = [];
    selectedAddon = null;
    activeTab = 'settings';
    auditLogs = [];
    auditOffset = 0;
    cachedUiData = null;
    cachedPermissionsData = null;
  }

  // Ladowanie listy addonow
  async function loadAddons() {
    try {
      addons = await ApiClient.get('/api/addons');
      renderGrid();
    } catch (err) {
      App.showToast(`${I18n.t('addons.install_error') || 'Blad ladowania addonow'}: ${err.message}`, 'error');
    }
  }

  // Renderowanie gridu kart addonow
  function renderGrid() {
    const grid = document.getElementById('addons-grid');
    if (!grid) return;

    if (!addons || addons.length === 0) {
      grid.innerHTML = `
        <div class="empty-state">
          <div class="empty-state-icon">
            <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5">
              <path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z"/>
              <polyline points="3.27 6.96 12 12.01 20.73 6.96"/><line x1="12" y1="22.08" x2="12" y2="12"/>
            </svg>
          </div>
          <div class="empty-state-text">${I18n.t('addons.empty') || 'Brak zainstalowanych addonow'}</div>
          <div class="empty-state-hint">${I18n.t('addons.empty_hint') || 'Kliknij "Zainstaluj addon" aby dodac pierwszy'}</div>
        </div>
      `;
      return;
    }

    grid.innerHTML = addons.map(addon => `
      <div class="addon-card ${addon.is_enabled ? '' : 'addon-disabled'}" data-addon-id="${escapeHtml(addon.addon_id)}">
        <div class="addon-card-header">
          <div class="addon-icon">
            <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5">
              <path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z"/>
            </svg>
          </div>
          <span class="addon-status-badge ${addon.is_enabled ? 'badge-success' : 'badge-disabled'}">
            ${addon.is_enabled ? (I18n.t('addons.enabled') || 'Aktywny') : (I18n.t('addons.disabled') || 'Wylaczony')}
          </span>
        </div>
        <div class="addon-card-body">
          <h4 class="addon-name">${escapeHtml(addon.name)}</h4>
          <p class="addon-desc">${escapeHtml(addon.description || '')}</p>
          <div class="addon-meta">
            <span class="addon-version">v${escapeHtml(addon.version)}</span>
            <span class="addon-author">${escapeHtml(addon.author || '')}</span>
          </div>
        </div>
      </div>
    `).join('');
  }

  // Klikniecie w karte addonu
  function handleGridClick(e) {
    const card = e.target.closest('.addon-card');
    if (!card) return;

    const addonId = card.dataset.addonId;
    selectedAddon = addons.find(a => a.addon_id === addonId);
    if (selectedAddon) {
      activeTab = 'settings';
      cachedUiData = null;
      cachedPermissionsData = null;
      renderDetail();
    }
  }

  // Renderowanie panelu szczegolowego
  async function renderDetail() {
    const panel = document.getElementById('addon-detail-panel');
    if (!panel || !selectedAddon) return;

    panel.hidden = false;

    // Pobierz dodatkowe dane
    if (!cachedUiData) {
      try {
        cachedUiData = await ApiClient.get(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/ui`);
      } catch (_) { cachedUiData = {}; }
    }
    if (!cachedPermissionsData) {
      try {
        cachedPermissionsData = await ApiClient.get(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/permissions`);
      } catch (_) { cachedPermissionsData = { declared_permissions: [], granted: [] }; }
    }

    panel.innerHTML = `
      <div class="addon-detail-header">
        <div class="addon-detail-title">
          <h3>${escapeHtml(selectedAddon.name)}</h3>
          <span class="addon-version">v${escapeHtml(selectedAddon.version)}</span>
          <span class="addon-status-badge ${selectedAddon.is_enabled ? 'badge-success' : 'badge-disabled'}">
            ${selectedAddon.is_enabled ? (I18n.t('addons.enabled') || 'Aktywny') : (I18n.t('addons.disabled') || 'Wylaczony')}
          </span>
        </div>
        <div class="addon-detail-actions">
          <button class="btn btn-sm ${selectedAddon.is_enabled ? 'btn-warning' : 'btn-success'}" data-action="toggle">
            ${selectedAddon.is_enabled ? (I18n.t('addons.disable') || 'Wylacz') : (I18n.t('addons.enable') || 'Wlacz')}
          </button>
          <button class="btn btn-sm btn-danger" data-action="uninstall">${I18n.t('addons.uninstall') || 'Odinstaluj'}</button>
          <button class="btn btn-sm btn-ghost" data-action="close">${I18n.t('common.close') || 'Zamknij'}</button>
        </div>
      </div>
      <div class="addon-tabs">
        <button class="addon-tab ${activeTab === 'settings' ? 'active' : ''}" data-tab="settings">${I18n.t('addons.settings') || 'Ustawienia'}</button>
        <button class="addon-tab ${activeTab === 'permissions' ? 'active' : ''}" data-tab="permissions">${I18n.t('addons.permissions') || 'Uprawnienia'}</button>
        <button class="addon-tab ${activeTab === 'resources' ? 'active' : ''}" data-tab="resources">${I18n.t('addons.resources') || 'Zasoby'}</button>
        <button class="addon-tab ${activeTab === 'logs' ? 'active' : ''}" data-tab="logs">${I18n.t('addons.logs') || 'Logi'}</button>
        <button class="addon-tab ${activeTab === 'network' ? 'active' : ''}" data-tab="network">${I18n.t('addons.network') || 'Siec'}</button>
        <button class="addon-tab ${activeTab === 'tools' ? 'active' : ''}" data-tab="tools">${I18n.t('addons.tools') || 'Narzedzia'}</button>
      </div>
      <div id="addon-tab-content" class="addon-tab-content">
        ${renderTabContent(activeTab)}
      </div>
    `;

    // Po wstawieniu HTML do DOM — zaladuj dane asynchroniczne dla aktywnej zakladki
    if (activeTab === 'resources') {
      loadResourceLimits();
    } else if (activeTab === 'logs') {
      loadAddonAuditLogs();
    } else if (activeTab === 'permissions') {
      loadGroupsForPermissions();
    } else if (activeTab === 'network') {
      loadNetworkRules();
    }
  }

  // Renderowanie zawartosci zakladki
  function renderTabContent(tab) {
    switch (tab) {
      case 'settings':
        return renderSettingsTab();
      case 'permissions':
        return renderPermissionsTab();
      case 'resources':
        return renderResourceLimitsTab();
      case 'logs':
        return renderLogsTab();
      case 'network':
        return renderNetworkTab();
      case 'tools':
        return renderToolsTab();
      default:
        return '';
    }
  }

  // ===========================================================================
  // Zakladka Ustawienia — formularz z config.schema z manifestu
  // ===========================================================================
  function renderSettingsTab() {
    const uiData = cachedUiData || {};
    const schema = uiData.config_schema || {};
    const values = uiData.config_values || {};

    // Schema moze byc obiektem z polami (kazdy klucz = pole konfiguracji)
    const fields = schema.fields || schema.properties || schema;

    if (!fields || Object.keys(fields).length === 0) {
      return `
        <div class="empty-state">
          <div class="empty-state-text">${I18n.t('addons.no_config') || 'Addon nie posiada konfiguracji'}</div>
        </div>
      `;
    }

    let formHtml = '<form id="addon-config-form" class="addon-config-form">';
    for (const [key, fieldDef] of Object.entries(fields)) {
      // Pomijaj zagniezdzone obiekty ktore nie sa definicja pola
      if (!fieldDef || typeof fieldDef !== 'object' || !fieldDef.type) continue;

      const currentValue = values[key];
      const defaultValue = fieldDef.default;
      const value = currentValue !== undefined ? currentValue : (defaultValue !== undefined ? defaultValue : '');
      const label = fieldDef.label || fieldDef.title || key;
      const fieldType = fieldDef.type || 'string';
      const description = fieldDef.description || '';
      const isRequired = fieldDef.required === true;

      formHtml += `<div class="form-group">`;
      formHtml += `<label for="cfg-${escapeHtml(key)}">`;
      formHtml += `${escapeHtml(label)}`;
      if (isRequired) formHtml += ` <span class="text-danger">*</span>`;
      formHtml += `</label>`;

      if (fieldType === 'boolean') {
        const checked = value === 'true' || value === true;
        formHtml += `
          <label class="toggle-switch">
            <input type="checkbox" id="cfg-${escapeHtml(key)}" data-config-key="${escapeHtml(key)}"
                   ${checked ? 'checked' : ''}>
            <span class="toggle-slider"></span>
          </label>
        `;
      } else if (fieldType === 'select' && fieldDef.options) {
        formHtml += `<select id="cfg-${escapeHtml(key)}" data-config-key="${escapeHtml(key)}" class="form-select">`;
        for (const opt of fieldDef.options) {
          const optVal = typeof opt === 'string' ? opt : opt.value;
          const optLabel = typeof opt === 'string' ? opt : opt.label;
          formHtml += `<option value="${escapeHtml(optVal)}" ${String(optVal) === String(value) ? 'selected' : ''}>${escapeHtml(optLabel)}</option>`;
        }
        formHtml += `</select>`;
      } else if (fieldType === 'text' || fieldType === 'textarea') {
        formHtml += `<textarea id="cfg-${escapeHtml(key)}" data-config-key="${escapeHtml(key)}" class="form-textarea" rows="4">${escapeHtml(String(value))}</textarea>`;
      } else {
        // Domyslnie: input type=text (dla "string", "number" itp.)
        const inputType = fieldType === 'number' ? 'number' : 'text';
        formHtml += `<input type="${inputType}" id="cfg-${escapeHtml(key)}" data-config-key="${escapeHtml(key)}" class="form-input" value="${escapeHtml(String(value))}"${isRequired ? ' required' : ''}>`;
      }

      if (description) {
        formHtml += `<small class="form-hint">${escapeHtml(description)}</small>`;
      }
      formHtml += `</div>`;
    }

    formHtml += `<button type="submit" class="btn btn-primary btn-sm">${I18n.t('addons.save_config') || 'Zapisz ustawienia'}</button>`;
    formHtml += `</form>`;

    // Przycisk OAuth — jesli addon wymaga autoryzacji OAuth (np. Teams, Outlook)
    const addonId = selectedAddon ? selectedAddon.addon_id : '';
    const oauthAddons = ['teams', 'outlook', 'onedrive', 'sharepoint'];
    if (oauthAddons.includes(addonId)) {
      formHtml += `
        <div style="margin-top: var(--spacing-lg); padding-top: var(--spacing-lg); border-top: 1px solid var(--color-border);">
          <h4 style="margin-bottom: var(--spacing-sm);">${I18n.t('addons.oauth_section') || 'Autoryzacja Microsoft OAuth'}</h4>
          <p style="margin-bottom: var(--spacing-sm); color: var(--color-text-secondary); font-size: var(--font-size-sm);">
            ${I18n.t('addons.oauth_hint') || 'Wymagana autoryzacja OAuth do korzystania z tej aplikacji.'}
          </p>
          <button class="btn btn-primary" data-action="oauth-login">
            ${I18n.t('addons.oauth_login') || 'Zaloguj do Microsoft'}
          </button>
        </div>
      `;
    }

    return formHtml;
  }

  // ===========================================================================
  // Zakladka Uprawnienia — per grupa, toggle boolean
  // ===========================================================================

  // Cache zaladowanych grup
  let cachedGroups = null;
  let selectedGroupId = null;

  function renderPermissionsTab() {
    const data = cachedPermissionsData || {};
    const declared = data.declared_permissions || [];
    const granted = data.granted || [];

    if (declared.length === 0) {
      return `<div class="empty-state"><div class="empty-state-text">${I18n.t('addons.no_permissions') || 'Aplikacja nie deklaruje uprawnien'}</div></div>`;
    }

    // Grupuj deklaracje po kategoriach
    const categories = {};
    for (const perm of declared) {
      const cat = perm.category || 'Inne';
      if (!categories[cat]) categories[cat] = [];
      categories[cat].push(perm);
    }

    // Lookup przyznanych uprawnien: "group:ID:permission_id" -> true
    const grantedLookup = {};
    for (const g of granted) {
      if (g.subject_type === 'group' && g.granted) {
        const key = `${g.subject_id}:${g.permission_id}`;
        grantedLookup[key] = true;
      }
    }

    let html = '<div class="permissions-section">';
    html += `<p style="margin-bottom: var(--spacing-md); color: var(--color-text-secondary); font-size: var(--font-size-sm);">
      ${I18n.t('addons.permissions_hint_groups') || 'Uprawnienia przyznawane per grupa uzytkownikow. Jesli uzytkownik jest w kilku grupach, wystarczy przyznanie w jednej.'}
    </p>`;

    // Wybor grupy — select zaladowany z API
    html += `
      <div style="margin-bottom: var(--spacing-lg); display: flex; gap: var(--spacing-sm); align-items: center;">
        <label style="font-weight: 600; white-space: nowrap;">${I18n.t('addons.perm_select_group') || 'Grupa'}:</label>
        <select id="perm-group-select" class="form-select" style="max-width: 300px;">
          <option value="">${I18n.t('addons.perm_loading_groups') || 'Ladowanie grup...'}</option>
        </select>
      </div>
    `;

    // Tabela uprawnien — toggles per uprawnienie
    html += '<div id="perm-table-container">';
    html += renderPermissionsTable(categories, grantedLookup, selectedGroupId);
    html += '</div>';

    // Przycisk zapisu
    html += `<div style="margin-top: var(--spacing-md);">`;
    html += `<button class="btn btn-primary btn-sm" data-action="save-all-perms">${I18n.t('addons.save_permissions') || 'Zapisz uprawnienia'}</button>`;
    html += `</div>`;

    html += '</div>';
    return html;
  }

  // Renderuje tabele uprawnien z toggle'ami ustawionymi na stan wybranej grupy
  function renderPermissionsTable(categories, grantedLookup, groupId) {
    let html = '<div class="table-wrapper"><table style="table-layout: fixed; width: 100%;">';
    html += `<colgroup><col style="width: 25%;"><col style="width: 55%;"><col style="width: 20%;"></colgroup>`;
    html += `<thead><tr>
      <th>${I18n.t('addons.perm_name') || 'Uprawnienie'}</th>
      <th>${I18n.t('common.description') || 'Opis'}</th>
      <th style="text-align: center;">${I18n.t('addons.perm_granted') || 'Przyznane'}</th>
    </tr></thead><tbody>`;

    for (const [categoryName, perms] of Object.entries(categories)) {
      html += `<tr><td colspan="3" style="padding: var(--spacing-sm) var(--spacing-md); background: var(--color-bg-secondary); font-weight: 600; font-size: var(--font-size-sm); text-transform: uppercase; letter-spacing: 0.05em; color: var(--color-text-secondary); border-bottom: 2px solid var(--color-border);">${escapeHtml(categoryName)}</td></tr>`;

      for (const perm of perms) {
        const permId = perm.id;
        const isGranted = groupId ? !!grantedLookup[`${groupId}:${permId}`] : false;
        const disabled = !groupId ? 'disabled' : '';
        html += `<tr>`;
        html += `<td><strong>${escapeHtml(perm.name)}</strong><br><code style="font-size: 0.8em; color: var(--color-text-secondary);">${escapeHtml(permId)}</code></td>`;
        html += `<td>${escapeHtml(perm.description)}</td>`;
        html += `<td style="text-align: center;">
          <label class="toggle-switch">
            <input type="checkbox" class="perm-toggle"
                   data-permission-id="${escapeHtml(permId)}"
                   ${isGranted ? 'checked' : ''} ${disabled}>
            <span class="toggle-slider"></span>
          </label>
        </td>`;
        html += `</tr>`;
      }
    }

    html += '</tbody></table></div>';
    return html;
  }

  // Stary widok uprawnien — dla addonow bez [[addon_permissions]]
  function renderLegacyPermissionsTab(permissions) {
    const perms = Array.isArray(permissions) ? permissions : [];

    return `
      <div class="permissions-section">
        <h4>${I18n.t('addons.permissions') || 'Uprawnienia dostepu'}</h4>
        <div class="table-wrapper">
          <table>
            <thead>
              <tr>
                <th>${I18n.t('common.type') || 'Typ'}</th>
                <th>ID</th>
                <th>${I18n.t('addons.perm_resource') || 'Zasob'}</th>
                <th>${I18n.t('addons.perm_level') || 'Poziom dostepu'}</th>
                <th>${I18n.t('common.actions') || 'Akcje'}</th>
              </tr>
            </thead>
            <tbody id="permissions-tbody">
              ${perms.length === 0
                ? `<tr><td colspan="5"><div class="empty-state"><div class="empty-state-text">${I18n.t('addons.no_permissions') || 'Brak skonfigurowanych uprawnien'}</div></div></td></tr>`
                : perms.map(p => `
                    <tr>
                      <td><span class="badge badge-${p.subject_type === 'user' ? 'info' : 'secondary'}">${escapeHtml(p.subject_type)}</span></td>
                      <td>${p.subject_id}</td>
                      <td>${escapeHtml(p.resource || '*')}</td>
                      <td>
                        <select class="form-select form-select-sm" data-perm-subject-type="${escapeHtml(p.subject_type)}"
                                data-perm-subject-id="${p.subject_id}" data-perm-resource="${escapeHtml(p.resource || '*')}">
                          <option value="none" ${p.access_level === 'none' ? 'selected' : ''}>${I18n.t('addons.perm_none') || 'Brak'}</option>
                          <option value="ro" ${p.access_level === 'ro' ? 'selected' : ''}>RO</option>
                          <option value="rw" ${p.access_level === 'rw' ? 'selected' : ''}>RW</option>
                          <option value="rwd" ${p.access_level === 'rwd' ? 'selected' : ''}>RWD</option>
                        </select>
                      </td>
                      <td>
                        <button class="btn btn-xs btn-primary" data-action="save-perm"
                                data-subject-type="${escapeHtml(p.subject_type)}"
                                data-subject-id="${p.subject_id}"
                                data-resource="${escapeHtml(p.resource || '*')}">${I18n.t('common.save') || 'Zapisz'}</button>
                      </td>
                    </tr>
                  `).join('')
              }
            </tbody>
          </table>
        </div>
        <div class="permissions-add" style="margin-top: 12px;">
          <h5>${I18n.t('addons.add_permission') || 'Dodaj uprawnienie'}</h5>
          <div class="inline-form">
            <select id="perm-add-type" class="form-select form-select-sm">
              <option value="user">${I18n.t('addons.perm_user') || 'Uzytkownik'}</option>
              <option value="group">${I18n.t('addons.perm_group') || 'Grupa'}</option>
            </select>
            <input type="number" id="perm-add-id" class="form-input form-input-sm" placeholder="ID">
            <input type="text" id="perm-add-resource" class="form-input form-input-sm" placeholder="${I18n.t('addons.perm_resource_placeholder') || 'Zasob (* = wszystkie)'}" value="*">
            <select id="perm-add-level" class="form-select form-select-sm">
              <option value="ro">RO</option>
              <option value="rw">RW</option>
              <option value="rwd">RWD</option>
            </select>
            <button class="btn btn-sm btn-primary" data-action="add-perm">${I18n.t('common.add') || 'Dodaj'}</button>
          </div>
        </div>
      </div>
    `;
  }

  // ===========================================================================
  // Zakladka Zasoby — limity CPU, RAM, GPU, storage
  // ===========================================================================
  function renderResourceLimitsTab() {
    if (!selectedAddon) return '';

    return `
      <div class="resource-limits-section">
        <h4>${I18n.t('addons.limits_title') || 'Limity zasobow'}</h4>
        <p style="margin-bottom: var(--spacing-md); color: var(--color-text-secondary); font-size: var(--font-size-sm);">
          ${I18n.t('addons.limits_hint') || '0 = bez limitu (unlimited)'}
        </p>
        <div id="resource-limits-form-container">
          <div class="empty-state">
            <div class="empty-state-text">${I18n.t('common.loading') || 'Ladowanie...'}</div>
          </div>
        </div>
      </div>
    `;
  }

  // Ladowanie limitow zasobow z API
  async function loadResourceLimits() {
    console.log('[Addons] loadResourceLimits called, selectedAddon:', selectedAddon?.addon_id);
    if (!selectedAddon) { console.log('[Addons] no selectedAddon'); return; }
    const container = document.getElementById('resource-limits-form-container');
    console.log('[Addons] container:', container);
    if (!container) { console.log('[Addons] no container element!'); return; }

    try {
      const data = await ApiClient.get(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/limits`);
      console.log('[Addons] limits data:', data);
      renderResourceLimitsForm(container, data);
    } catch (err) {
      console.error('[Addons] limits error:', err);
      container.innerHTML = `<div class="empty-state"><div class="empty-state-text">${I18n.t('common.error') || 'Blad'}: ${err.message}</div></div>`;
    }
  }

  // Renderowanie formularza limitow zasobow
  function renderResourceLimitsForm(container, data) {
    const labels = data.labels || {};
    const hint = I18n.t('addons.limits_hint') || '0 = bez limitu (unlimited)';

    // Presety fuel z API (lub domyslne)
    const fuelPresets = data.fuel_presets || {
      light: { value: 1000000, label: 'Lekki (1M) — proste narzedzia' },
      standard: { value: 10000000, label: 'Standardowy (10M) — typowe addony' },
      intensive: { value: 100000000, label: 'Intensywny (100M) — ciezkie obliczenia' },
      unlimited: { value: 0, label: 'Nieograniczony — zaufane addony' }
    };

    // Sprawdz czy aktualna wartosc pasuje do presetu
    const currentFuel = data.fuel_limit !== undefined ? data.fuel_limit : 0;
    const presetValues = Object.values(fuelPresets).map(p => p.value);
    const isCustomFuel = currentFuel > 0 && !presetValues.includes(currentFuel);

    const fields = [
      { key: 'max_instances', type: 'number', label: I18n.t('addons.max_instances') || labels.max_instances || 'Maks. instancji' },
      { key: 'fuel_limit', type: 'fuel_preset', label: I18n.t('addons.fuel_limit') || labels.fuel_limit || 'Limit obliczen (fuel per wywolanie)' },
      { key: 'ram_limit_mb', type: 'number', label: I18n.t('addons.ram_limit') || labels.ram_limit_mb || 'Limit RAM (MB)' },
      { key: 'gpu_enabled', type: 'boolean', label: I18n.t('addons.gpu_enabled') || labels.gpu_enabled || 'Dostep do GPU' },
      { key: 'vram_limit_mb', type: 'number', label: I18n.t('addons.vram_limit') || labels.vram_limit_mb || 'Limit VRAM (MB)' },
      { key: 'storage_limit_mb', type: 'number', label: I18n.t('addons.storage_limit') || labels.storage_limit_mb || 'Limit storage (MB)' },
      { key: 'http_requests_per_min', type: 'number', label: I18n.t('addons.http_limit') || labels.http_requests_per_min || 'Limit HTTP zadan/min' },
      { key: 'llm_tokens_per_min', type: 'number', label: I18n.t('addons.llm_tokens_limit') || labels.llm_tokens_per_min || 'Limit tokenow LLM/min' },
    ];

    let html = '<form id="resource-limits-form">';

    for (const field of fields) {
      const value = data[field.key];
      html += `<div class="form-group">`;
      html += `<label for="rl-${field.key}">${escapeHtml(field.label)}</label>`;

      if (field.type === 'boolean') {
        const checked = value === true || value === 1;
        html += `
          <label class="toggle-switch">
            <input type="checkbox" id="rl-${field.key}" data-limit-key="${field.key}" ${checked ? 'checked' : ''}>
            <span class="toggle-slider"></span>
          </label>
        `;
      } else if (field.type === 'fuel_preset') {
        // Dropdown z presetami + opcja wlasna
        html += `<select id="rl-fuel-preset" data-limit-key="fuel_limit_preset" class="form-select">`;
        for (const [presetKey, preset] of Object.entries(fuelPresets)) {
          const selected = (!isCustomFuel && currentFuel === preset.value) ? 'selected' : '';
          html += `<option value="${preset.value}" ${selected}>${escapeHtml(preset.label)}</option>`;
        }
        const customSelected = isCustomFuel ? 'selected' : '';
        html += `<option value="custom" ${customSelected}>${I18n.t('addons.fuel_custom') || 'Wlasny...'}</option>`;
        html += `</select>`;
        const customDisplay = isCustomFuel ? '' : 'display:none;';
        const customValue = isCustomFuel ? currentFuel : '';
        html += `<input type="number" id="rl-fuel-custom" class="form-input" style="${customDisplay} margin-top:8px;" placeholder="${I18n.t('addons.fuel_custom_placeholder') || 'Wpisz wartosc fuel'}" min="100000" value="${customValue}">`;
        html += `<small class="form-hint">${I18n.t('addons.fuel_hint') || 'Fuel = liczba instrukcji WASM na jedno wywolanie. 1M = proste operacje, 10M = typowa praca, 100M = intensywne obliczenia.'}</small>`;
      } else {
        html += `<input type="number" id="rl-${field.key}" data-limit-key="${field.key}" class="form-input" value="${value !== undefined ? value : 0}" min="0">`;
        html += `<small class="form-hint">${hint}</small>`;
      }

      html += `</div>`;
    }

    html += `<button type="button" class="btn btn-primary btn-sm" data-action="save-limits">${I18n.t('common.save') || 'Zapisz'}</button>`;
    html += '</form>';

    container.innerHTML = html;

    // Obsluga zmiany presetu fuel — pokaz/ukryj pole wlasnej wartosci
    const presetSelect = document.getElementById('rl-fuel-preset');
    const customInput = document.getElementById('rl-fuel-custom');
    if (presetSelect && customInput) {
      presetSelect.addEventListener('change', () => {
        if (presetSelect.value === 'custom') {
          customInput.style.display = '';
          customInput.focus();
        } else {
          customInput.style.display = 'none';
          customInput.value = '';
        }
      });
    }
  }

  // Zapis limitow zasobow
  async function saveResourceLimits() {
    if (!selectedAddon) return;

    const form = document.getElementById('resource-limits-form');
    if (!form) return;

    const values = {};
    form.querySelectorAll('[data-limit-key]').forEach(el => {
      const key = el.dataset.limitKey;
      // Pomin fuel_limit_preset — obslugujemy recznie
      if (key === 'fuel_limit_preset') return;
      if (el.type === 'checkbox') {
        values[key] = el.checked;
      } else {
        values[key] = parseInt(el.value, 10) || 0;
      }
    });

    // Oblicz fuel_limit z presetu lub pola wlasnej wartosci
    const presetSelect = document.getElementById('rl-fuel-preset');
    const customInput = document.getElementById('rl-fuel-custom');
    if (presetSelect) {
      if (presetSelect.value === 'custom') {
        values.fuel_limit = parseInt(customInput?.value, 10) || 0;
      } else {
        values.fuel_limit = parseInt(presetSelect.value, 10) || 0;
      }
    }

    try {
      await ApiClient.put(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/limits`, values);
      App.showToast(I18n.t('addons.limits_saved') || 'Limity zasobow zapisane', 'success');
    } catch (err) {
      App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
    }
  }

  // ===========================================================================
  // Zakladka Logi — audit log filtrowany per addon
  // ===========================================================================
  function renderLogsTab() {
    return `
      <div class="audit-logs-section">
        <div class="table-wrapper">
          <table>
            <thead>
              <tr>
                <th>${I18n.t('addons.log_time') || 'Czas'}</th>
                <th>${I18n.t('addons.log_user') || 'Uzytkownik'}</th>
                <th>${I18n.t('addons.log_action') || 'Akcja'}</th>
                <th>${I18n.t('addons.log_resource') || 'Zasob'}</th>
                <th>${I18n.t('addons.log_details') || 'Szczegoly'}</th>
              </tr>
            </thead>
            <tbody id="addon-audit-tbody">
              <tr><td colspan="5"><div class="empty-state"><div class="empty-state-text">${I18n.t('common.loading') || 'Ladowanie...'}</div></div></td></tr>
            </tbody>
          </table>
        </div>
        <div class="pagination" id="addon-audit-pagination"></div>
      </div>
    `;
  }

  // Zaladowanie logow audytowych per addon
  async function loadAddonAuditLogs() {
    if (!selectedAddon) return;
    try {
      auditLogs = await ApiClient.get(
        `/api/audit?addon_id=${encodeURIComponent(selectedAddon.addon_id)}&offset=${auditOffset}&limit=${AUDIT_PAGE_SIZE}`
      );
      renderAuditLogRows();
    } catch (err) {
      const tbody = document.getElementById('addon-audit-tbody');
      if (tbody) {
        tbody.innerHTML = `<tr><td colspan="5"><div class="empty-state"><div class="empty-state-text">${I18n.t('addons.log_error') || 'Blad ladowania logow'}</div></div></td></tr>`;
      }
    }
  }

  // Renderowanie wierszy tabeli logow audytowych
  function renderAuditLogRows() {
    const tbody = document.getElementById('addon-audit-tbody');
    if (!tbody) return;

    if (!auditLogs || auditLogs.length === 0) {
      tbody.innerHTML = `<tr><td colspan="5"><div class="empty-state"><div class="empty-state-text">${I18n.t('addons.no_logs') || 'Brak logow'}</div></div></td></tr>`;
      return;
    }

    tbody.innerHTML = auditLogs.map(log => `
      <tr>
        <td>${escapeHtml(log.timestamp || '')}</td>
        <td>${log.user_id || '-'}</td>
        <td><span class="badge badge-info">${escapeHtml(log.action)}</span></td>
        <td>${escapeHtml(log.resource || '-')}</td>
        <td>${escapeHtml(log.details || '-')}</td>
      </tr>
    `).join('');

    // Paginacja
    const pag = document.getElementById('addon-audit-pagination');
    if (pag) {
      pag.innerHTML = `
        <button class="btn btn-xs btn-ghost" data-action="audit-prev" ${auditOffset === 0 ? 'disabled' : ''}>${I18n.t('common.prev') || 'Poprzednia'}</button>
        <span class="pagination-info">${I18n.t('common.page') || 'Strona'} ${Math.floor(auditOffset / AUDIT_PAGE_SIZE) + 1}</span>
        <button class="btn btn-xs btn-ghost" data-action="audit-next" ${auditLogs.length < AUDIT_PAGE_SIZE ? 'disabled' : ''}>${I18n.t('common.next') || 'Nastepna'}</button>
      `;
    }
  }

  // ===========================================================================
  // Zakladka Narzedzia — lista tools z manifest (nazwa, opis, parametry)
  // ===========================================================================
  function renderToolsTab() {
    const uiData = cachedUiData || {};
    const tools = uiData.tools || [];
    const skillMd = uiData.skill_md || '';

    if (tools.length === 0 && !skillMd) {
      return `
        <div class="empty-state">
          <div class="empty-state-text">${I18n.t('addons.no_tools') || 'Addon nie posiada zarejestrowanych narzedzi'}</div>
        </div>
      `;
    }

    let html = '';

    if (tools.length > 0) {
      html += `<h4>${I18n.t('addons.tools') || 'Narzedzia'} (${tools.length})</h4>`;
      html += '<div class="table-wrapper"><table>';
      html += `<thead><tr>
        <th>${I18n.t('common.name') || 'Nazwa'}</th>
        <th>${I18n.t('common.description') || 'Opis'}</th>
        <th>${I18n.t('addons.tool_params') || 'Parametry'}</th>
      </tr></thead><tbody>`;

      for (const tool of tools) {
        const fn_obj = tool.function || tool;
        const name = fn_obj.name || tool.name || '';
        const desc = fn_obj.description || tool.description || '';
        const params = fn_obj.parameters || tool.parameters || {};

        // Formatuj parametry jako czytelny JSON
        let paramsHtml = '-';
        if (params && Object.keys(params).length > 0) {
          const required = params.required || [];
          const properties = params.properties || {};
          if (Object.keys(properties).length > 0) {
            paramsHtml = '<div class="tool-params">';
            for (const [pName, pDef] of Object.entries(properties)) {
              const isReq = required.includes(pName);
              paramsHtml += `<div class="tool-param">
                <code>${escapeHtml(pName)}</code>
                <span class="badge badge-${isReq ? 'warning' : 'info'}" style="font-size: 0.7em;">${pDef.type || 'any'}${isReq ? '*' : ''}</span>
                ${pDef.description ? `<small>${escapeHtml(pDef.description)}</small>` : ''}
              </div>`;
            }
            paramsHtml += '</div>';
          } else {
            paramsHtml = `<pre style="font-size: 0.8em; margin: 0;">${escapeHtml(JSON.stringify(params, null, 2))}</pre>`;
          }
        }

        html += `<tr>
          <td><code style="font-weight: 600;">${escapeHtml(name)}</code></td>
          <td>${escapeHtml(desc)}</td>
          <td>${paramsHtml}</td>
        </tr>`;
      }

      html += '</tbody></table></div>';
    }

    if (skillMd) {
      html += `<h4 style="margin-top: 16px;">SKILL.md</h4>`;
      html += `<pre class="skill-md-preview">${escapeHtml(skillMd)}</pre>`;
    }

    return html;
  }

  // ===========================================================================
  // Zakladka Siec — reguly sieciowe addonu (approve/revoke)
  // ===========================================================================

  // Renderowanie zakladki Siec
  function renderNetworkTab() {
    return `
      <div class="network-rules-section">
        <h4>${I18n.t('addons.network_title') || 'Reguly sieciowe'}</h4>
        <p style="margin-bottom: var(--spacing-md); color: var(--color-text-secondary); font-size: var(--font-size-sm);">
          ${I18n.t('addons.network_hint') || 'Addon deklaruje wymagane polaczenia sieciowe. Administrator musi zatwierdzic kazda regule przed uzyciem.'}
        </p>
        <div id="network-rules-container">
          <div class="empty-state">
            <div class="empty-state-text">${I18n.t('common.loading') || 'Ladowanie...'}</div>
          </div>
        </div>
      </div>
    `;
  }

  // Ladowanie regul sieciowych z API
  async function loadNetworkRules() {
    const container = document.getElementById('network-rules-container');
    if (!container || !selectedAddon) return;

    try {
      const response = await ApiClient.get(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/network-rules`);
      const rules = Array.isArray(response) ? response : (response && Array.isArray(response.network_rules) ? response.network_rules : []);
      renderNetworkRulesTable(container, rules);
    } catch (err) {
      container.innerHTML = `<div class="empty-state"><div class="empty-state-text">${err.message}</div></div>`;
    }
  }

  // Renderowanie tabeli regul sieciowych
  function renderNetworkRulesTable(container, rules) {
    if (rules.length === 0) {
      container.innerHTML = `<div class="empty-state"><div class="empty-state-text">${I18n.t('addons.network_no_rules') || 'Addon nie deklaruje regul sieciowych'}</div></div>`;
      return;
    }

    let html = '<div class="table-wrapper"><table>';
    html += `<thead><tr>
      <th>${I18n.t('addons.network_rule_id') || 'ID reguly'}</th>
      <th>${I18n.t('addons.network_protocol') || 'Protokol'}</th>
      <th>${I18n.t('addons.network_host_port') || 'Host:Port'}</th>
      <th>${I18n.t('common.description') || 'Opis'}</th>
      <th>${I18n.t('addons.network_required') || 'Wymagane'}</th>
      <th>${I18n.t('addons.network_status') || 'Status'}</th>
      <th>${I18n.t('common.actions') || 'Akcje'}</th>
    </tr></thead><tbody>`;

    for (const rule of rules) {
      const isApproved = rule.approved === 1 || rule.approved === true;
      const isPending = !isApproved && rule.approved !== -1 && rule.approved !== 'revoked';
      const isRevoked = rule.approved === -1 || rule.approved === 'revoked';

      // Badge statusu
      let statusBadge = '';
      if (isApproved) {
        statusBadge = `<span class="badge badge-success">${I18n.t('addons.enabled') || 'Zatwierdzone'}</span>`;
      } else if (isRevoked) {
        statusBadge = `<span class="badge badge-danger">${I18n.t('addons.network_revoked') || 'Cofniete'}</span>`;
      } else {
        statusBadge = `<span class="badge badge-warning">${I18n.t('addons.network_pending') || 'Oczekuje'}</span>`;
      }

      // Przycisk akcji
      let actionBtn = '';
      if (isApproved) {
        actionBtn = `<button class="btn btn-xs btn-warning" data-action="revoke-rule" data-rule-id="${escapeHtml(String(rule.rule_id || rule.id))}">${I18n.t('addons.network_revoke') || 'Cofnij'}</button>`;
      } else {
        actionBtn = `<button class="btn btn-xs btn-success" data-action="approve-rule" data-rule-id="${escapeHtml(String(rule.rule_id || rule.id))}">${I18n.t('addons.network_approve') || 'Zatwierdz'}</button>`;
      }

      // Wymagane — tak/nie
      const requiredLabel = rule.required ? (I18n.t('common.yes') || 'Tak') : (I18n.t('common.no') || 'Nie');

      html += `<tr>
        <td><code>${escapeHtml(String(rule.rule_id || rule.id || ''))}</code></td>
        <td>${escapeHtml(rule.protocol || '-')}</td>
        <td>${escapeHtml(rule.host || '')}${rule.port ? ':' + escapeHtml(String(rule.port)) : ''}</td>
        <td>${escapeHtml(rule.description || '-')}</td>
        <td>${requiredLabel}</td>
        <td>${statusBadge}</td>
        <td>${actionBtn}</td>
      </tr>`;
    }

    html += '</tbody></table></div>';
    container.innerHTML = html;
  }

  // Zatwierdzenie reguly sieciowej
  async function handleApproveRule(ruleId) {
    if (!selectedAddon) return;
    try {
      await ApiClient.put(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/network-rules/${encodeURIComponent(ruleId)}/approve`);
      App.showToast(I18n.t('addons.network_approved') || 'Regula zatwierdzona', 'success');
      loadNetworkRules();
    } catch (err) {
      App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
    }
  }

  // Cofniecie reguly sieciowej
  async function handleRevokeRule(ruleId) {
    if (!selectedAddon) return;
    try {
      await ApiClient.put(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/network-rules/${encodeURIComponent(ruleId)}/revoke`);
      App.showToast(I18n.t('addons.network_revoked') || 'Regula cofnieta', 'success');
      loadNetworkRules();
    } catch (err) {
      App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
    }
  }

  // ===========================================================================
  // Obsluga zdarzen
  // ===========================================================================

  // Obsluga klikniec w panelu szczegolowym
  async function handleDetailClick(e) {
    const action = e.target.dataset.action;
    const tab = e.target.dataset.tab;

    // Przelaczenie zakladki
    if (tab) {
      activeTab = tab;
      renderDetail();
      return;
    }

    if (!selectedAddon) return;

    switch (action) {
      case 'close':
        closeDetail();
        break;

      case 'toggle':
        await toggleAddon(selectedAddon.addon_id, !selectedAddon.is_enabled);
        break;

      case 'uninstall':
        if (confirm(I18n.t('addons.uninstall_confirm') || `Czy na pewno chcesz odinstalowac addon "${selectedAddon.name}"?`)) {
          await uninstallAddon(selectedAddon.addon_id);
        }
        break;

      case 'add-perm':
        await addPermission();
        break;

      case 'save-perm':
        await savePermission(e.target);
        break;

      case 'save-all-perms':
        await saveAllPermissions();
        break;

      case 'save-limits':
        await saveResourceLimits();
        break;

      case 'revoke-perm':
        await revokePermission(e.target);
        break;

      case 'oauth-login':
        window.open(`/api/addons/${selectedAddon.addon_id}/oauth/login`, '_blank');
        break;

      case 'approve-rule':
        await handleApproveRule(e.target.dataset.ruleId);
        break;

      case 'revoke-rule':
        await handleRevokeRule(e.target.dataset.ruleId);
        break;

      case 'audit-prev':
        auditOffset = Math.max(0, auditOffset - AUDIT_PAGE_SIZE);
        loadAddonAuditLogs();
        break;

      case 'audit-next':
        auditOffset += AUDIT_PAGE_SIZE;
        loadAddonAuditLogs();
        break;
    }
  }

  // Obsluga zdarzen change (selecty uprawnien)
  function handleDetailChange(e) {
    // Zmiana wybranej grupy w zakladce uprawnien
    if (e.target.id === 'perm-group-select') {
      const groupId = parseInt(e.target.value, 10);
      selectedGroupId = isNaN(groupId) ? null : groupId;
      updatePermissionsToggles();
    }
  }

  // Laduje grupy z API i wypelnia select
  async function loadGroupsForPermissions() {
    const select = document.getElementById('perm-group-select');
    if (!select) return;

    try {
      if (!cachedGroups) {
        cachedGroups = await ApiClient.get('/api/groups');
      }
      const groups = cachedGroups || [];

      select.innerHTML = `<option value="">-- ${I18n.t('addons.perm_choose_group') || 'Wybierz grupe'} --</option>`;
      for (const g of groups) {
        const selected = selectedGroupId === g.id ? 'selected' : '';
        select.innerHTML += `<option value="${g.id}" ${selected}>${escapeHtml(g.name)}${g.description ? ' — ' + escapeHtml(g.description) : ''}</option>`;
      }

      if (selectedGroupId) {
        updatePermissionsToggles();
      }
    } catch (err) {
      select.innerHTML = `<option value="">Blad ladowania grup</option>`;
    }
  }

  // Aktualizuje stan toggleow na podstawie wybranej grupy
  function updatePermissionsToggles() {
    const data = cachedPermissionsData || {};
    const granted = data.granted || [];

    // Lookup per ta grupa
    const grantedLookup = {};
    for (const g of granted) {
      if (g.subject_type === 'group' && g.subject_id === selectedGroupId && g.granted) {
        grantedLookup[g.permission_id] = true;
      }
    }

    const toggles = document.querySelectorAll('.perm-toggle');
    for (const toggle of toggles) {
      const permId = toggle.dataset.permissionId;
      toggle.checked = !!grantedLookup[permId];
      toggle.disabled = !selectedGroupId;
    }
  }

  // Obsluga submit formularza
  async function handleDetailSubmit(e) {
    if (e.target.id === 'addon-config-form') {
      e.preventDefault();
      await saveConfig();
    }
  }

  // Zamkniecie panelu szczegolowego
  function closeDetail() {
    const panel = document.getElementById('addon-detail-panel');
    if (panel) panel.hidden = true;
    selectedAddon = null;
    cachedUiData = null;
    cachedPermissionsData = null;
  }

  // Wlaczenie/Wylaczenie addonu
  async function toggleAddon(addonId, enabled) {
    try {
      await ApiClient.put(`/api/addons/${encodeURIComponent(addonId)}`, { enabled });
      App.showToast(enabled ? (I18n.t('addons.enabled') || 'Addon wlaczony') : (I18n.t('addons.disabled') || 'Addon wylaczony'), 'success');
      await loadAddons();
      selectedAddon = addons.find(a => a.addon_id === addonId);
      cachedUiData = null;
      cachedPermissionsData = null;
      if (selectedAddon) renderDetail();
    } catch (err) {
      App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
    }
  }

  // Odinstalowanie addonu
  async function uninstallAddon(addonId) {
    try {
      await ApiClient.delete(`/api/addons/${encodeURIComponent(addonId)}`);
      App.showToast(I18n.t('addons.uninstalled') || 'Addon odinstalowany', 'success');
      closeDetail();
      await loadAddons();
    } catch (err) {
      App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
    }
  }

  // Upload ZIP z addonem
  async function handleFileUpload(e) {
    const file = e.target.files[0];
    if (!file) return;

    try {
      App.showToast(I18n.t('addons.installing') || 'Instalowanie addonu...', 'info');

      const arrayBuffer = await file.arrayBuffer();
      const token = ApiClient.getToken();

      const response = await fetch('/api/addons/install', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${token}`,
        },
        body: arrayBuffer,
      });

      if (!response.ok) {
        const errData = await response.json().catch(() => ({}));
        throw new Error(errData.error || `HTTP ${response.status}`);
      }

      const result = await response.json();
      App.showToast(`${I18n.t('addons.install_success') || 'Addon zainstalowany'}: "${result.display_name || result.addon_id}"`, 'success');
      await loadAddons();
    } catch (err) {
      App.showToast(`${I18n.t('addons.install_error') || 'Blad instalacji'}: ${err.message}`, 'error');
    }

    // Wyczysc input
    e.target.value = '';
  }

  // Zapis konfiguracji addonu
  async function saveConfig() {
    if (!selectedAddon) return;

    const form = document.getElementById('addon-config-form');
    if (!form) return;

    const values = {};
    form.querySelectorAll('[data-config-key]').forEach(el => {
      const key = el.dataset.configKey;
      if (el.type === 'checkbox') {
        values[key] = el.checked ? 'true' : 'false';
      } else {
        values[key] = el.value;
      }
    });

    try {
      await ApiClient.put(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/config`, { values });
      App.showToast(I18n.t('addons.config_saved') || 'Konfiguracja zapisana', 'success');
      // Odswierz cache
      cachedUiData = null;
    } catch (err) {
      App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
    }
  }

  // Zapis wszystkich granularnych uprawnien (boolean: przyznane/nieprzyznane)
  async function saveAllPermissions() {
    if (!selectedAddon) return;

    if (!selectedGroupId) {
      App.showToast(I18n.t('addons.perm_choose_group_first') || 'Najpierw wybierz grupe', 'error');
      return;
    }

    const subjectType = 'group';
    const subjectId = selectedGroupId;

    const toggles = document.querySelectorAll('.perm-toggle');
    let saved = 0;
    let errors = 0;

    for (const toggle of toggles) {
      const permissionId = toggle.dataset.permissionId;
      const granted = toggle.checked;

      try {
        await ApiClient.put(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/permissions`, {
          subject_type: subjectType,
          subject_id: subjectId,
          permission_id: permissionId,
          granted: granted,
        });
        saved++;
      } catch (err) {
        errors++;
      }
    }

    if (errors > 0) {
      App.showToast(`${I18n.t('addons.perm_partial_error') || 'Czesc uprawnien nie zostala zapisana'} (${errors} ${I18n.t('addons.perm_errors') || 'bledow'})`, 'warning');
    } else if (saved > 0) {
      App.showToast(`${I18n.t('addons.perm_saved') || 'Uprawnienia zapisane'} (${saved})`, 'success');
    } else {
      App.showToast(I18n.t('addons.perm_nothing') || 'Brak uprawnien do zapisania', 'info');
    }

    // Odswierz dane
    cachedPermissionsData = null;
    renderDetail();
  }

  // Cofniecie uprawnienia
  async function revokePermission(btn) {
    if (!selectedAddon) return;

    const subjectType = btn.dataset.subjectType;
    const subjectId = parseInt(btn.dataset.subjectId, 10);
    const permissionId = btn.dataset.resource;

    try {
      await ApiClient.put(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/permissions`, {
        subject_type: subjectType,
        subject_id: subjectId,
        permission_id: permissionId,
        granted: false,
      });
      App.showToast(I18n.t('addons.perm_revoked') || 'Uprawnienie cofniete', 'success');
      cachedPermissionsData = null;
      renderDetail();
    } catch (err) {
      App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
    }
  }

  // Dodanie uprawnienia (stary tryb)
  async function addPermission() {
    if (!selectedAddon) return;

    const subjectType = document.getElementById('perm-add-type').value;
    const subjectId = parseInt(document.getElementById('perm-add-id').value, 10);
    const permissionId = document.getElementById('perm-add-resource').value || '*';

    if (isNaN(subjectId)) {
      App.showToast(I18n.t('addons.perm_enter_id') || 'Podaj poprawne ID podmiotu', 'error');
      return;
    }

    try {
      await ApiClient.put(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/permissions`, {
        subject_type: subjectType,
        subject_id: subjectId,
        permission_id: permissionId,
        granted: true,
      });
      App.showToast(I18n.t('addons.perm_added') || 'Uprawnienie dodane', 'success');
      cachedPermissionsData = null;
      renderDetail();
    } catch (err) {
      App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
    }
  }

  // Zapis zmienionego uprawnienia (stary tryb)
  async function savePermission(btn) {
    if (!selectedAddon) return;

    const subjectType = btn.dataset.subjectType;
    const subjectId = parseInt(btn.dataset.subjectId, 10);
    const permissionId = btn.dataset.resource;

    // Znajdz checkbox w tym samym wierszu
    const row = btn.closest('tr');
    const checkbox = row.querySelector('input[type="checkbox"]');
    const granted = checkbox ? checkbox.checked : false;

    try {
      await ApiClient.put(`/api/addons/${encodeURIComponent(selectedAddon.addon_id)}/permissions`, {
        subject_type: subjectType,
        subject_id: subjectId,
        permission_id: permissionId,
        granted: granted,
      });
      App.showToast(I18n.t('addons.perm_updated') || 'Uprawnienie zaktualizowane', 'success');
    } catch (err) {
      App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
    }
  }

  // Escapowanie HTML
  function escapeHtml(str) {
    if (!str) return '';
    const div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
  }

  return { render, mount, unmount };
})();
