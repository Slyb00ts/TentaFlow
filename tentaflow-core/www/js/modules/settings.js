// =============================================================================
// Plik: modules/settings.js
// Opis: Ekran Ustawienia z zakladkami administracyjnymi (tf-tabs underline):
//       1) Ogólne       — surowe pary klucz/wartosc z binary protocol
//       2) SSO / OIDC   — CRUD providerow SSO
//       3) OAuth        — oauth_redirect_base_url
//       4) TLS          — tls_cert_pem, tls_key_pem
//       5) Dostępy zewnętrzne — hf_token, ngc_api_key, rejestry kontenerów
//      Wszystkie klucze bazy danych sa snake_case (tabela settings). Sekcja
//      "Ogólne" filtruje klucze obslugiwane w dedykowanych zakladkach oraz
//      klucze flow_*/speaker_*/voice_*/enrollment_* aby uniknac duplikacji.
//      CRUD settings/SSO/TLS/dostępów idzie przez binary WS (ApiBinary); REST
//      pozostal tylko dla OAuth flow (/api/sso/login, /api/sso/callback) i
//      testu NGC (/api/nim/catalog).
// =============================================================================

import { byId, escapeHtml, escapeAttr, toast, formatDate, formatRelative, formatBytes } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { TfWindow } from '/js/components/tf-window.js';
import { renderMeshTab, bindMeshTab } from '/js/modules/settings-network.js';
import {
  loadStorageOverview,
  renderStorageTab as renderStorageDataTab,
  bindStorageTab as bindStorageDataTab,
} from '/js/modules/settings-storage.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-table.js';

// --- Klucze obslugiwane w dedykowanych zakladkach (ukryte w "Ogólne") ---
const DEDICATED_KEYS = new Set([
  'hf_token',
  'oauth_redirect_base_url',
  'tls_cert_pem',
  'tls_key_pem',
  'ngc_api_key',
]);

// Klucze martwe — nie pokazywane w zakladce Ogólne (czekaja na usuniecie z DB
// przez usera). Flow engine zostaje w DB bo backend moze z niego korzystac, ale
// GUI nie wystawia dedykowanej zakladki.
const DEAD_KEY_PREFIXES = [
  'speaker_',
  'voice_',
  'enrollment_',
  'flow_engine',
  'flow_debug',
  'flow_default',
];

function isDeadKey(key) {
  const k = key.toLowerCase();
  return DEAD_KEY_PREFIXES.some((p) => k.startsWith(p));
}

// --- Stan modulu ---
let currentTab = 'general';
let settings = {};            // { key: { value, isSecret } }
let ssoProviders = [];
let registries = [];
let syncConflicts = [];
let syncConflictsStatus = 'open';
let syncConflictsAddonId = 'contacts';
const CONFIGURED_SECRET_MASK = '••••••••••••';

const SSO_TYPES = [
  { value: 'azure_ad', label: 'Azure AD' },
  { value: 'google', label: 'Google' },
  { value: 'adfs', label: 'ADFS' },
  { value: 'authentik', label: 'Authentik' },
  { value: 'oidc', label: 'Generic OIDC' },
];

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

const SettingsScreen = {
  get title() { return I18n.t('settings.title'); },

  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('settings')} ${escapeHtml(I18n.t('settings.title'))}</h1>
        </div>
      </div>

      <tf-tabs variant="underline" value="${currentTab}" id="settings-tabs">
        <tf-tab id="general" icon="settings">${escapeHtml(I18n.t('settings.tab_general'))}</tf-tab>
        <tf-tab id="sso" icon="users">${escapeHtml(I18n.t('settings.tab_sso'))}</tf-tab>
        <tf-tab id="oauth" icon="share">${escapeHtml(I18n.t('settings.tab_oauth'))}</tf-tab>
        <tf-tab id="tls" icon="mesh-admin">${escapeHtml(I18n.t('settings.tab_tls'))}</tf-tab>
        <tf-tab id="mesh" icon="network">${escapeHtml(I18n.t('settings.tab_mesh'))}</tf-tab>
        <tf-tab id="sync" icon="refresh">Sync</tf-tab>
        <tf-tab id="storage" icon="database">${escapeHtml(I18n.t('settings.tab_storage') || 'Magazyn danych')}</tf-tab>
        <tf-tab id="external" icon="key">${escapeHtml(I18n.t('settings.tab_external_access') || 'Dostępy zewnętrzne')}</tf-tab>
      </tf-tabs>

      <div id="settings-tab-body"></div>
    `;
  },

  async mount() {
    byId('settings-tabs')?.addEventListener('change', handleTabChange);
    await loadAll();
    renderTab();
  },

  unmount() {
    settings = {};
    ssoProviders = [];
    registries = [];
    syncConflicts = [];
  },
};

// --- Pobranie wszystkich danych ---

async function loadAll() {
  try {
    const [settingsResp, ssoResp, registriesResp] = await Promise.all([
      ApiBinary.one('settingsListRequest').catch(() => ({ entries: [] })),
      ApiBinary.one('ssoProvidersListRequest').catch(() => ({ providers: [] })),
      ApiBinary.list('registryListRequest').catch(() => []),
    ]);
    settings = {};
    for (const row of settingsResp.entries || []) {
      settings[row.key] = { value: row.value, isSecret: !!row.isSecret };
    }
    ssoProviders = ssoResp.providers || [];
    registries = Array.isArray(registriesResp) ? registriesResp : [];
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

function getSetting(key, dflt = '') {
  const v = settings[key]?.value;
  return v != null ? v : dflt;
}

function hasConfiguredSetting(key) {
  const value = getSetting(key, '').trim();
  return value.length > 0;
}

function configuredSecretValue(configured) {
  return configured ? CONFIGURED_SECRET_MASK : '';
}

function readSecretInputValue(id) {
  const value = byId(id)?.value?.trim() || '';
  return value === CONFIGURED_SECRET_MASK ? '' : value;
}

function bindConfiguredSecretInput(id, configured) {
  const input = byId(id);
  if (!input || !configured) return;
  input.addEventListener('focus', () => {
    if (input.value === CONFIGURED_SECRET_MASK) input.value = '';
  });
  input.addEventListener('blur', () => {
    if (!input.value.trim()) input.value = CONFIGURED_SECRET_MASK;
  });
}

async function saveSettingKey(key, value) {
  const isSecret = /secret|key|password|token|master/i.test(key);
  await ApiBinary.action('settingsUpdateRequest', {
    entries: [{ key, value: String(value), isSecret }],
  });
  settings[key] = { value: String(value), isSecret };
}

// --- Taby ---

function handleTabChange(e) {
  const id = e.detail?.value;
  if (!id || id === currentTab) return;
  currentTab = id;
  renderTab();
}

function renderTab() {
  const host = byId('settings-tab-body');
  if (!host) return;
  switch (currentTab) {
    case 'general': host.innerHTML = renderGeneralTab(); bindGeneralTab(); break;
    case 'sso': host.innerHTML = renderSsoTab(); bindSsoTab(); break;
    case 'oauth': host.innerHTML = renderOauthTab(); bindOauthTab(); break;
    case 'tls': host.innerHTML = renderTlsTab(); bindTlsTab(); break;
    case 'mesh':
      host.innerHTML = `<div class="empty-big" style="padding:24px;">${escapeHtml(I18n.t('common.loading'))}</div>`;
      renderMeshTab().then((html) => {
        host.innerHTML = html;
        bindMeshTab(host, () => { renderTab(); });
      }).catch((err) => {
        host.innerHTML = `<div class="empty-big" style="padding:24px;color:var(--danger);">${escapeHtml(err.message || String(err))}</div>`;
      });
      break;
    case 'sync': host.innerHTML = renderSyncTab(); bindSyncTab(); void loadSyncConflicts(); break;
    case 'storage': void loadStorageDataTab(); break;
    case 'external': host.innerHTML = renderExternalAccessTab(); bindExternalAccessTab(); break;
  }
}

// ==========================================================================
// Zakladka: Sync
// ==========================================================================

function renderSyncTab() {
  return `
    <div class="card">
      <div class="card-header">
        <h3>Konflikty synchronizacji</h3>
        <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap;">
          <tf-input id="sync-addon-id" value="${escapeAttr(syncConflictsAddonId)}" placeholder="contacts"></tf-input>
          <tf-select id="sync-status" value="${escapeAttr(syncConflictsStatus)}">
            <option value="open">open</option>
            <option value="resolved">resolved</option>
            <option value="ignored">ignored</option>
            <option value="superseded">superseded</option>
          </tf-select>
          <tf-button variant="ghost" size="sm" icon="refresh" id="sync-refresh">Odśwież</tf-button>
        </div>
      </div>
      <div class="card-body">
        <p class="form-hint" style="margin:0 0 12px;">
          Konflikty pochodzą z lokalnej tabeli addonu <code>__tentaflow_sync_conflicts</code>. Akcje resolve idą przez binary protocol i wymagają roli admina.
        </p>
        <tf-table id="sync-conflicts-table" sortable>
          <tf-column key="resource" label="Zasób" renderer="html" sortable></tf-column>
          <tf-column key="operation" label="Operacja" renderer="html"></tf-column>
          <tf-column key="source" label="Źródło" sortable></tf-column>
          <tf-column key="error" label="Błąd" renderer="html"></tf-column>
          <tf-column key="statusChip" label="Status" renderer="chip" sortable></tf-column>
          <tf-column key="actions" label="Akcje" renderer="html"></tf-column>
        </tf-table>
      </div>
    </div>
  `;
}

function bindSyncTab() {
  byId('sync-refresh')?.addEventListener('click', loadSyncConflicts);
  byId('sync-addon-id')?.addEventListener('change', () => {
    syncConflictsAddonId = byId('sync-addon-id')?.value?.trim() || 'contacts';
    void loadSyncConflicts();
  });
  byId('sync-status')?.addEventListener('change', () => {
    syncConflictsStatus = byId('sync-status')?.value || 'open';
    void loadSyncConflicts();
  });
  renderSyncConflictsTable();
}

async function loadSyncConflicts() {
  const addonId = byId('sync-addon-id')?.value?.trim() || syncConflictsAddonId || 'contacts';
  const status = byId('sync-status')?.value || syncConflictsStatus || 'open';
  syncConflictsAddonId = addonId;
  syncConflictsStatus = status;
  try {
    const resp = await ApiBinary.one('syncConflictsListRequest', {
      orgId: 'org-default',
      addonId,
      status,
      limit: 100,
    });
    syncConflicts = Array.isArray(resp.conflicts) ? resp.conflicts : [];
    renderSyncConflictsTable();
  } catch (err) {
    syncConflicts = [];
    renderSyncConflictsTable();
    toast(`Sync: ${err.message}`, 'error');
  }
}

function renderSyncConflictsTable() {
  const table = byId('sync-conflicts-table');
  if (!table) return;
  table.rows = syncConflicts.map((row) => {
    const operationId = row.operationId || row.operation_id || '';
    const status = row.status || 'open';
    const resourceType = row.resourceType || row.resource_type || '';
    const resourceId = row.resourceId || row.resource_id || '';
    const tableName = row.tableName || row.table_name || '';
    const sourceNodeId = row.sourceNodeId || row.source_node_id || '';
    const errorKind = row.errorKind || row.error_kind || '';
    const errorMessage = row.errorMessage || row.error_message || '';
    return {
      resource: `
        <strong>${escapeHtml(resourceType || tableName || 'resource')}</strong>
        <div class="muted"><code>${escapeHtml(resourceId || operationId.slice(0, 16))}</code></div>
      `,
      operation: `
        <span>${escapeHtml(row.action || '')}</span>
        <div class="muted"><code>${escapeHtml(operationId.slice(0, 24))}</code></div>
      `,
      source: sourceNodeId ? sourceNodeId.slice(0, 16) : 'local',
      error: `
        <strong>${escapeHtml(errorKind)}</strong>
        <div class="muted">${escapeHtml(errorMessage)}</div>
      `,
      statusChip: conflictStatusChip(status),
      actions: renderSyncConflictActions(operationId, status),
    };
  });
  if (!table._syncConflictActionBound) {
    table._syncConflictActionBound = true;
    table.shadowRoot?.addEventListener('click', async (event) => {
      const btn = event.target.closest?.('[data-sync-resolution]');
      if (!btn) return;
      await resolveSyncConflict(btn.dataset.syncOperation, btn.dataset.syncResolution);
    });
  }
}

function conflictStatusChip(status) {
  const chipStatus = status === 'open' ? 'warn' : status === 'resolved' ? 'ok' : 'info';
  return { status: chipStatus, label: status || 'unknown' };
}

function renderSyncConflictActions(operationId, status) {
  if (status !== 'open') return '<span class="muted">—</span>';
  const op = escapeAttr(operationId);
  return `
    <div style="display:flex;gap:6px;justify-content:flex-end;flex-wrap:wrap;">
      <tf-button variant="ghost" size="sm" icon="check" data-sync-operation="${op}" data-sync-resolution="keep_local">Local</tf-button>
      <tf-button variant="ghost" size="sm" icon="x" data-sync-operation="${op}" data-sync-resolution="ignore">Ignore</tf-button>
      <tf-button variant="primary" size="sm" icon="download" data-sync-operation="${op}" data-sync-resolution="accept_remote">Remote</tf-button>
    </div>
  `;
}

async function resolveSyncConflict(operationId, resolution) {
  if (!operationId || !resolution) return;
  try {
    await ApiBinary.one('syncConflictResolveRequest', {
      orgId: 'org-default',
      addonId: syncConflictsAddonId,
      operationId,
      resolution,
    });
    toast(`Konflikt rozwiązany: ${resolution}`, 'success');
    await loadSyncConflicts();
  } catch (err) {
    toast(`Resolve conflict: ${err.message}`, 'error');
  }
}

// ==========================================================================
// Zakladka: Magazyn danych (settings-storage.js)
// ==========================================================================

// Laduje overview (kategorie + dysk) i renderuje zakladke; reload odswieza.
async function loadStorageDataTab() {
  const host = byId('settings-tab-body');
  if (!host || currentTab !== 'storage') return;
  host.innerHTML = renderStorageDataTab(null);
  try {
    const overview = await loadStorageOverview();
    if (currentTab !== 'storage') return;
    host.innerHTML = renderStorageDataTab(overview);
    bindStorageDataTab(loadStorageDataTab);
  } catch (err) {
    host.innerHTML = `<div class="empty-big" style="padding:24px;color:var(--danger);">${escapeHtml(err.message || String(err))}</div>`;
  }
}

// ==========================================================================
// Zakladka: Ogólne
// ==========================================================================

function filteredGeneralEntries() {
  return Object.entries(settings)
    .filter(([key]) => !DEDICATED_KEYS.has(key) && !isDeadKey(key))
    .sort(([a], [b]) => a.localeCompare(b));
}

function renderGeneralTab() {
  const entries = filteredGeneralEntries();
  const rows = entries.length === 0
    ? `<tr><td colspan="4"><div class="empty-big" style="padding:24px;">${escapeHtml(I18n.t('settings.general_empty'))}</div></td></tr>`
    : entries.map(([key, s]) => {
      const isSecret = s.isSecret || /secret|key|password|token|master/i.test(key);
      const value = s.value == null ? '' : s.value;
      return `
        <tr data-key="gen-${escapeAttr(key)}">
          <td data-label="${escapeAttr(I18n.t('settings.key'))}"><code style="font-size:12px;">${escapeHtml(key)}</code></td>
          <td data-label="${escapeAttr(I18n.t('settings.value'))}">
            <tf-input
              type="${isSecret ? 'password' : 'text'}"
              value="${escapeAttr(value)}"
              data-general-key="${escapeAttr(key)}"
              placeholder="${isSecret ? '***' : ''}"
            ></tf-input>
          </td>
          <td data-label="${escapeAttr(I18n.t('settings.last_change'))}" style="font-size:11px;color:var(--text-3);white-space:nowrap;">${s.updatedAt ? escapeHtml(formatDate(s.updatedAt)) : '—'}</td>
          <td style="text-align:right;">
            <tf-button variant="primary" size="sm" icon="check" data-general-save="${escapeAttr(key)}">${escapeHtml(I18n.t('common.save'))}</tf-button>
          </td>
        </tr>
      `;
    }).join('');

  return `
    <div class="card">
      <div class="card-header">
        <h3>${escapeHtml(I18n.t('settings.general_title'))}</h3>
        <tf-button variant="ghost" size="sm" icon="refresh" id="general-refresh">${escapeHtml(I18n.t('settings.refresh'))}</tf-button>
      </div>
      <div class="card-body">
        <p class="form-hint" style="margin:0 0 12px;">${escapeHtml(I18n.t('settings.general_hint'))}</p>
        <table class="data-table">
          <thead>
            <tr>
              <th>${escapeHtml(I18n.t('settings.key'))}</th>
              <th>${escapeHtml(I18n.t('settings.value'))}</th>
              <th>${escapeHtml(I18n.t('settings.last_change'))}</th>
              <th style="text-align:right;">${escapeHtml(I18n.t('common.actions'))}</th>
            </tr>
          </thead>
          <tbody>${rows}</tbody>
        </table>
      </div>
    </div>
  `;
}

function bindGeneralTab() {
  byId('general-refresh')?.addEventListener('click', async () => {
    await loadAll();
    renderTab();
  });
  document.querySelectorAll('[data-general-save]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const key = btn.dataset.generalSave;
      const input = document.querySelector(`[data-general-key="${CSS.escape(key)}"]`);
      const value = input?.value ?? '';
      try {
        await saveSettingKey(key, value);
        toast(I18n.t('settings.save_success', { key }), 'success');
      } catch (err) {
        toast(I18n.t('settings.save_error', { error: err.message }), 'error');
      }
    });
  });
}

// ==========================================================================
// Zakladka: SSO / OIDC
// ==========================================================================

function renderSsoTab() {
  const typeLabel = (t) => SSO_TYPES.find((x) => x.value === t)?.label ?? t;
  const rows = ssoProviders.length === 0
    ? `<tr><td colspan="6"><div class="empty-big" style="padding:24px;">${escapeHtml(I18n.t('settings.sso_empty'))}</div></td></tr>`
    : ssoProviders.map((p) => `
      <tr>
        <td data-label="${escapeAttr(I18n.t('settings.sso_name'))}"><strong>${escapeHtml(p.name)}</strong></td>
        <td data-label="${escapeAttr(I18n.t('settings.sso_type'))}">${escapeHtml(typeLabel(p.providerType))}</td>
        <td data-label="${escapeAttr(I18n.t('settings.sso_discovery'))}"><code style="font-size:11px;">${escapeHtml(p.discoveryUrl || '')}</code></td>
        <td data-label="${escapeAttr(I18n.t('settings.sso_auto_create'))}">${p.autoCreateUsers ? '<tf-chip status="ok">' + escapeHtml(I18n.t('common.yes')) + '</tf-chip>' : '<tf-chip status="info">' + escapeHtml(I18n.t('common.no')) + '</tf-chip>'}</td>
        <td data-label="${escapeAttr(I18n.t('common.status'))}">${p.enabled ? '<tf-chip status="ok">' + escapeHtml(I18n.t('settings.sso_active')) + '</tf-chip>' : '<tf-chip status="warn">' + escapeHtml(I18n.t('settings.sso_disabled')) + '</tf-chip>'}</td>
        <td style="text-align:right;">
          <tf-button variant="danger" size="sm" icon="trash" data-sso-delete="${p.id}" title="${escapeAttr(I18n.t('common.delete'))}"></tf-button>
        </td>
      </tr>
    `).join('');

  return `
    <div class="card">
      <div class="card-header">
        <h3>${escapeHtml(I18n.t('settings.sso_title'))}</h3>
        <tf-button variant="ghost" size="sm" icon="refresh" id="sso-refresh">${escapeHtml(I18n.t('settings.refresh'))}</tf-button>
      </div>
      <div class="card-body">
        <p class="form-hint" style="margin:0 0 12px;">${escapeHtml(I18n.t('settings.sso_hint'))}</p>
        <table class="data-table">
          <thead>
            <tr>
              <th>${escapeHtml(I18n.t('settings.sso_name'))}</th>
              <th>${escapeHtml(I18n.t('settings.sso_type'))}</th>
              <th>${escapeHtml(I18n.t('settings.sso_discovery'))}</th>
              <th>${escapeHtml(I18n.t('settings.sso_auto_create'))}</th>
              <th>${escapeHtml(I18n.t('common.status'))}</th>
              <th style="text-align:right;">${escapeHtml(I18n.t('common.actions'))}</th>
            </tr>
          </thead>
          <tbody>${rows}</tbody>
        </table>
      </div>
    </div>

    <div class="card" style="margin-top:16px;">
      <div class="card-header">
        <h3>${escapeHtml(I18n.t('settings.sso_add_title'))}</h3>
      </div>
      <div class="card-body">
        <div class="form-row">
          <tf-input id="sso-name" label="${escapeAttr(I18n.t('settings.sso_name'))}" placeholder="Azure AD Firma"></tf-input>
        </div>
        <div class="form-row">
          <span class="tf-label">${escapeHtml(I18n.t('settings.sso_type'))}</span>
          <tf-select id="sso-type" value="azure_ad">
            ${SSO_TYPES.map((t) => `<option value="${escapeAttr(t.value)}">${escapeHtml(t.label)}</option>`).join('')}
          </tf-select>
        </div>
        <div class="form-row">
          <tf-input id="sso-client-id" label="${escapeAttr(I18n.t('settings.sso_client_id'))}"></tf-input>
        </div>
        <div class="form-row">
          <tf-input id="sso-client-secret" type="password" label="${escapeAttr(I18n.t('settings.sso_client_secret'))}"></tf-input>
        </div>
        <div class="form-row">
          <tf-input id="sso-discovery-url" label="${escapeAttr(I18n.t('settings.sso_discovery'))}" placeholder="https://login.microsoftonline.com/{tenant}/v2.0" hint="${escapeAttr(I18n.t('settings.sso_discovery_url_hint'))}"></tf-input>
        </div>
        <div class="form-row" style="display:flex;align-items:center;gap:12px;">
          <tf-toggle id="sso-auto-create"></tf-toggle>
          <div>
            <div><strong>${escapeHtml(I18n.t('settings.sso_auto_create'))}</strong></div>
            <div class="form-hint">${escapeHtml(I18n.t('settings.sso_auto_create_hint'))}</div>
          </div>
        </div>
        <div class="form-row">
          <tf-input id="sso-default-group" type="number" label="${escapeAttr(I18n.t('settings.sso_default_group'))}" hint="${escapeAttr(I18n.t('settings.sso_default_group_hint'))}"></tf-input>
        </div>
        <div style="margin-top:16px;">
          <tf-button variant="primary" icon="plus" id="sso-add">${escapeHtml(I18n.t('common.add'))}</tf-button>
        </div>
      </div>
    </div>
  `;
}

function bindSsoTab() {
  byId('sso-refresh')?.addEventListener('click', async () => {
    try {
      const resp = await ApiBinary.one('ssoProvidersListRequest');
      ssoProviders = resp.providers || [];
    } catch (_) {
      ssoProviders = [];
    }
    renderTab();
  });

  byId('sso-add')?.addEventListener('click', async () => {
    const name = byId('sso-name')?.value?.trim() || '';
    const providerType = byId('sso-type')?.value || 'azure_ad';
    const clientId = byId('sso-client-id')?.value?.trim() || '';
    const clientSecret = byId('sso-client-secret')?.value?.trim() || '';
    const discoveryUrl = byId('sso-discovery-url')?.value?.trim() || '';
    const autoCreateUsers = byId('sso-auto-create')?.hasAttribute('checked') ?? false;
    const defaultGroupStr = byId('sso-default-group')?.value?.trim() || '';
    const defaultGroupId = defaultGroupStr || null;

    if (!name || !clientId || !clientSecret || !discoveryUrl) {
      toast(I18n.t('settings.sso_add_required'), 'error');
      return;
    }

    try {
      await ApiBinary.action('ssoProviderCreateRequest', {
        name,
        providerType,
        clientId,
        clientSecret,
        discoveryUrl,
        autoCreateUsers,
        defaultGroupId,
      });
      toast(I18n.t('settings.sso_add_success'), 'success');
      const resp = await ApiBinary.one('ssoProvidersListRequest').catch(() => ({ providers: [] }));
      ssoProviders = resp.providers || [];
      renderTab();
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });

  document.querySelectorAll('[data-sso-delete]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const id = parseInt(btn.dataset.ssoDelete, 10);
      if (!Number.isFinite(id)) return;
      try {
        await ApiBinary.action('ssoProviderDeleteRequest', { id });
        const resp = await ApiBinary.one('ssoProvidersListRequest').catch(() => ({ providers: [] }));
        ssoProviders = resp.providers || [];
        renderTab();
      } catch (err) {
        toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
      }
    });
  });
}

// ==========================================================================
// Zakladka: OAuth Redirect URL
// ==========================================================================

function renderOauthTab() {
  const url = getSetting('oauth_redirect_base_url', 'https://localhost:8090');
  return `
    <div class="card">
      <div class="card-header">
        <h3>${escapeHtml(I18n.t('settings.oauth_title'))}</h3>
      </div>
      <div class="card-body">
        <p class="form-hint" style="margin:0 0 16px;">${escapeHtml(I18n.t('settings.oauth_hint'))}</p>
        <div class="form-row">
          <tf-input id="oauth-url" label="${escapeAttr(I18n.t('settings.oauth_url_label'))}" value="${escapeAttr(url)}" placeholder="${escapeAttr(I18n.t('settings.oauth_url_placeholder'))}"></tf-input>
        </div>
        <div style="margin-top:16px;">
          <tf-button variant="primary" icon="check" id="oauth-save">${escapeHtml(I18n.t('common.save'))}</tf-button>
        </div>
      </div>
    </div>
  `;
}

function bindOauthTab() {
  byId('oauth-save')?.addEventListener('click', async () => {
    const value = byId('oauth-url')?.value?.trim() || '';
    if (!value) {
      toast(I18n.t('common.required'), 'error');
      return;
    }
    if (!value.startsWith('http://') && !value.startsWith('https://')) {
      toast(I18n.t('settings.oauth_url_invalid'), 'error');
      return;
    }
    try {
      await saveSettingKey('oauth_redirect_base_url', value);
      toast(I18n.t('settings.oauth_saved'), 'success');
    } catch (err) {
      toast(I18n.t('settings.save_error', { error: err.message }), 'error');
    }
  });
}

// ==========================================================================
// Zakladka: TLS
// ==========================================================================

function renderTlsTab() {
  // Backend maskuje certyfikat/klucz jako "<redacted>" w listingu settings.
  // Do statusu TLS uzywamy dedykowanego tlsStatusRequest (binary) aby nie
  // opierac sie na maskowanym polu value.
  const certMasked = getSetting('tls_cert_pem', '');
  const keyMasked = getSetting('tls_key_pem', '');
  // Jesli backend maskuje, nie pokazuj tego jako tresc PEM w textarea.
  const cleanMask = (v) => (v === '<redacted>' || v === '***' ? '' : v);
  const certValue = cleanMask(certMasked);
  const keyValue = cleanMask(keyMasked);

  const placeholderStatus = `<tf-chip status="info" id="tls-status-chip">…</tf-chip>`;

  return `
    <div class="card">
      <div class="card-header">
        <h3>${escapeHtml(I18n.t('settings.tls.title'))} ${placeholderStatus}</h3>
      </div>
      <div class="card-body">
        <p class="form-hint" style="margin:0 0 16px;">${escapeHtml(I18n.t('settings.tls.subtitle'))}</p>
        <div style="display:flex;gap:16px;flex-wrap:wrap;">
          <div style="flex:1;min-width:280px;">
            <label class="tf-label">${escapeHtml(I18n.t('settings.tls.cert_label'))}</label>
            <input type="file" id="tls-cert-file" accept=".pem,.crt,.key" style="margin-bottom:8px;">
            <textarea id="tls-cert-pem" rows="10" placeholder="-----BEGIN CERTIFICATE-----..." style="width:100%;font-family:monospace;font-size:11px;resize:vertical;padding:8px;background:var(--bg-2);color:var(--text-1);border:1px solid var(--border-1);border-radius:4px;">${escapeHtml(certValue)}</textarea>
          </div>
          <div style="flex:1;min-width:280px;">
            <label class="tf-label">${escapeHtml(I18n.t('settings.tls.key_label'))}</label>
            <input type="file" id="tls-key-file" accept=".pem,.crt,.key" style="margin-bottom:8px;">
            <textarea id="tls-key-pem" rows="10" placeholder="-----BEGIN PRIVATE KEY-----..." style="width:100%;font-family:monospace;font-size:11px;resize:vertical;padding:8px;background:var(--bg-2);color:var(--text-1);border:1px solid var(--border-1);border-radius:4px;">${escapeHtml(keyValue)}</textarea>
          </div>
        </div>
        <div style="display:flex;gap:8px;margin-top:16px;">
          <tf-button variant="primary" icon="check" id="tls-save">${escapeHtml(I18n.t('settings.tls.save'))}</tf-button>
        </div>
      </div>
    </div>
  `;
}

function bindTlsTab() {
  // Refresh chip ze statusem po stronie serwera (binary).
  (async () => {
    try {
      const { hasCert, hasKey } = await ApiBinary.one('tlsStatusRequest');
      const chip = byId('tls-status-chip');
      if (!chip) return;
      if (hasCert && hasKey) {
        chip.setAttribute('status', 'ok');
        chip.textContent = I18n.t('settings.tls_active');
      } else {
        chip.setAttribute('status', 'warn');
        chip.textContent = I18n.t('settings.tls_missing');
      }
    } catch (_) {
      // Jesli status niedostepny, zostaw placeholder.
    }
  })();

  byId('tls-cert-file')?.addEventListener('change', (e) => {
    const file = e.target.files[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = (ev) => { byId('tls-cert-pem').value = ev.target.result; };
    reader.readAsText(file);
  });
  byId('tls-key-file')?.addEventListener('change', (e) => {
    const file = e.target.files[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = (ev) => { byId('tls-key-pem').value = ev.target.result; };
    reader.readAsText(file);
  });

  byId('tls-save')?.addEventListener('click', async () => {
    const cert = byId('tls-cert-pem')?.value?.trim() || '';
    const key = byId('tls-key-pem')?.value?.trim() || '';
    if (!cert || !key) {
      toast(I18n.t('settings.tls_save_required'), 'error');
      return;
    }
    try {
      await saveSettingKey('tls_cert_pem', cert);
      await saveSettingKey('tls_key_pem', key);
      toast(I18n.t('settings.tls.save_success'), 'success');
      await loadAll();
      renderTab();
    } catch (err) {
      toast(I18n.t('settings.tls.save_error', { error: err.message }), 'error');
    }
  });
}

// ==========================================================================
// Zakladka: Dostępy zewnętrzne
// ==========================================================================

function renderRegistryRows() {
  return registries.length === 0
    ? `<tr><td colspan="3"><div class="empty-big" style="padding:24px;">${escapeHtml(I18n.t('registries.empty'))}</div></td></tr>`
    : registries.map((r) => `
      <tr>
        <td data-label="${escapeAttr(I18n.t('registries.col_url'))}"><code>${escapeHtml(r.url)}</code></td>
        <td data-label="${escapeAttr(I18n.t('registries.col_type'))}"><tf-chip status="accent">${escapeHtml(r.kind)}</tf-chip></td>
        <td data-label="${escapeAttr(I18n.t('registries.col_auth'))}">${r.authRequired ? `<tf-chip status="warn">${escapeHtml(I18n.t('registries.auth_yes'))}</tf-chip>` : `<tf-chip status="ok">${escapeHtml(I18n.t('registries.auth_no'))}</tf-chip>`}</td>
      </tr>
    `).join('');
}

function renderExternalAccessTab() {
  const hfConfigured = hasConfiguredSetting('hf_token');
  const ngcConfigured = hasConfiguredSetting('ngc_api_key');
  const hfChip = hfConfigured
    ? `<tf-chip status="ok">${escapeHtml(I18n.t('settings.external_configured'))}</tf-chip>`
    : `<tf-chip status="warn">${escapeHtml(I18n.t('settings.external_not_configured'))}</tf-chip>`;
  const ngcPlaceholderChip = `<tf-chip status="info" id="ngc-status-chip">…</tf-chip>`;

  return `
    <div class="card">
      <div class="card-header">
        <h3>${escapeHtml(I18n.t('settings.external_hf_title'))} ${hfChip}</h3>
      </div>
      <div class="card-body">
        <p class="form-hint" style="margin:0 0 16px;">${escapeHtml(I18n.t('settings.external_hf_hint'))}</p>
        <div class="form-row">
          <tf-input id="hf-token" type="password" label="${escapeAttr(I18n.t('settings.external_hf_token'))}" placeholder="hf_..." value="${escapeAttr(configuredSecretValue(hfConfigured))}"></tf-input>
        </div>
        <div style="display:flex;gap:8px;margin-top:16px;">
          <tf-button variant="primary" icon="check" id="hf-save">${escapeHtml(I18n.t('common.save'))}</tf-button>
        </div>
      </div>
    </div>

    <div class="card">
      <div class="card-header">
        <h3>${escapeHtml(I18n.t('settings.ngc_title'))} ${ngcPlaceholderChip}</h3>
      </div>
      <div class="card-body">
        <p class="form-hint" style="margin:0 0 16px;">${escapeHtml(I18n.t('settings.ngc_hint'))}</p>
        <div class="form-row">
          <tf-input id="ngc-key" type="password" label="${escapeAttr(I18n.t('settings.ngc_key_label'))}" placeholder="nvapi-..." value="${escapeAttr(configuredSecretValue(ngcConfigured))}"></tf-input>
        </div>
        <div style="display:flex;gap:8px;margin-top:16px;">
          <tf-button variant="primary" icon="check" id="ngc-save">${escapeHtml(I18n.t('common.save'))}</tf-button>
          <tf-button variant="ghost" icon="refresh" id="ngc-test">${escapeHtml(I18n.t('settings.ngc_test'))}</tf-button>
        </div>
      </div>
    </div>

    <div class="card">
      <div class="card-header">
        <h3>${escapeHtml(I18n.t('registries.title'))}</h3>
        <tf-button variant="ghost" size="sm" icon="refresh" id="registries-refresh">${escapeHtml(I18n.t('settings.refresh'))}</tf-button>
      </div>
      <div class="card-body">
        <table class="data-table">
          <thead>
            <tr>
              <th>${escapeHtml(I18n.t('registries.col_url'))}</th>
              <th>${escapeHtml(I18n.t('registries.col_type'))}</th>
              <th>${escapeHtml(I18n.t('registries.col_auth'))}</th>
            </tr>
          </thead>
          <tbody>${renderRegistryRows()}</tbody>
        </table>
      </div>
    </div>
  `;
}

function bindExternalAccessTab() {
  bindConfiguredSecretInput('hf-token', hasConfiguredSetting('hf_token'));
  bindConfiguredSecretInput('ngc-key', hasConfiguredSetting('ngc_api_key'));

  (async () => {
    try {
      const { configured } = await ApiBinary.one('ngcStatusRequest');
      const chip = byId('ngc-status-chip');
      if (!chip) return;
      if (configured) {
        chip.setAttribute('status', 'ok');
        chip.textContent = I18n.t('settings.ngc_configured');
      } else {
        chip.setAttribute('status', 'warn');
        chip.textContent = I18n.t('settings.ngc_not_configured');
      }
    } catch (_) {
      // brak danych — placeholder zostaje
    }
  })();

  byId('hf-save')?.addEventListener('click', async () => {
    const value = readSecretInputValue('hf-token');
    if (!value) {
      toast(I18n.t('settings.external_hf_save_empty'), 'error');
      return;
    }
    try {
      await saveSettingKey('hf_token', value);
      byId('hf-token').value = '';
      toast(I18n.t('settings.save_success', { key: 'hf_token' }), 'success');
      await loadAll();
      renderTab();
    } catch (err) {
      toast(I18n.t('settings.save_error', { error: err.message }), 'error');
    }
  });

  byId('ngc-save')?.addEventListener('click', async () => {
    const value = readSecretInputValue('ngc-key');
    if (!value) {
      toast(I18n.t('settings.ngc_save_empty'), 'error');
      return;
    }
    try {
      await saveSettingKey('ngc_api_key', value);
      byId('ngc-key').value = '';
      toast(I18n.t('settings.save_success', { key: 'ngc_api_key' }), 'success');
      await loadAll();
      renderTab();
    } catch (err) {
      toast(I18n.t('settings.save_error', { error: err.message }), 'error');
    }
  });

  byId('ngc-test')?.addEventListener('click', async () => {
    const btn = byId('ngc-test');
    if (btn) btn.setAttribute('disabled', '');
    try {
      // Probkujemy katalog NIM zeby potwierdzic ze klucz NGC jest akceptowany.
      const resp = await ApiBinary.one('nimCatalogListRequest');
      if (resp?.error) {
        toast(`${I18n.t('settings.ngc_test_failed')}: ${resp.error}`, 'error');
      } else {
        toast(I18n.t('settings.ngc_test_success'), 'success');
      }
    } catch (err) {
      toast(`${I18n.t('settings.ngc_test_failed')}: ${err.message}`, 'error');
    } finally {
      if (btn) btn.removeAttribute('disabled');
    }
  });

  byId('registries-refresh')?.addEventListener('click', async () => {
    registries = await ApiBinary.list('registryListRequest').catch(() => []);
    renderTab();
  });
}


export default SettingsScreen;
