// =============================================================================
// Plik: modules/settings.js
// Opis: Ekran Ustawienia — 5 zakladek (tf-tabs underline):
//       1) Ogólne       — surowe pary klucz/wartosc z binary protocol
//       2) SSO / OIDC   — CRUD providerow SSO
//       3) OAuth        — oauth_redirect_base_url
//       4) TLS          — tls_cert_pem, tls_key_pem
//       5) NVIDIA NGC   — ngc_api_key, test polaczenia przez /api/nim/catalog
//      Wszystkie klucze bazy danych sa snake_case (tabela settings). Sekcja
//      "Ogólne" filtruje klucze obslugiwane w dedykowanych zakladkach oraz
//      klucze flow_*/speaker_*/voice_*/enrollment_* aby uniknac duplikacji.
//      CRUD settings/SSO/TLS/NGC idzie przez binary WS (ApiBinary); REST
//      pozostal tylko dla OAuth flow (/api/sso/login, /api/sso/callback) i
//      testu NGC (/api/nim/catalog).
// =============================================================================

import { byId, escapeHtml, escapeAttr, toast, formatDate } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';

// --- Klucze obslugiwane w dedykowanych zakladkach (ukryte w "Ogólne") ---
const DEDICATED_KEYS = new Set([
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
        <tf-tab id="ngc" icon="model">${escapeHtml(I18n.t('settings.tab_ngc'))}</tf-tab>
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
  },
};

// --- Pobranie wszystkich danych ---

async function loadAll() {
  try {
    const [settingsResp, ssoResp] = await Promise.all([
      ApiBinary.one('settingsListRequest').catch(() => ({ entries: [] })),
      ApiBinary.one('ssoProvidersListRequest').catch(() => ({ providers: [] })),
    ]);
    settings = {};
    for (const row of settingsResp.entries || []) {
      settings[row.key] = { value: row.value, isSecret: !!row.isSecret };
    }
    ssoProviders = ssoResp.providers || [];
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

function getSetting(key, dflt = '') {
  const v = settings[key]?.value;
  return v != null ? v : dflt;
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
    case 'ngc': host.innerHTML = renderNgcTab(); bindNgcTab(); break;
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
          <td><code style="font-size:12px;">${escapeHtml(key)}</code></td>
          <td>
            <tf-input
              type="${isSecret ? 'password' : 'text'}"
              value="${escapeAttr(value)}"
              data-general-key="${escapeAttr(key)}"
              placeholder="${isSecret ? '***' : ''}"
            ></tf-input>
          </td>
          <td style="font-size:11px;color:var(--text-3);white-space:nowrap;">${s.updatedAt ? escapeHtml(formatDate(s.updatedAt)) : '—'}</td>
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
        <td><strong>${escapeHtml(p.name)}</strong></td>
        <td>${escapeHtml(typeLabel(p.providerType))}</td>
        <td><code style="font-size:11px;">${escapeHtml(p.discoveryUrl || '')}</code></td>
        <td>${p.autoCreateUsers ? '<tf-chip status="ok">' + escapeHtml(I18n.t('common.yes')) + '</tf-chip>' : '<tf-chip status="info">' + escapeHtml(I18n.t('common.no')) + '</tf-chip>'}</td>
        <td>${p.enabled ? '<tf-chip status="ok">' + escapeHtml(I18n.t('settings.sso_active')) + '</tf-chip>' : '<tf-chip status="warn">' + escapeHtml(I18n.t('settings.sso_disabled')) + '</tf-chip>'}</td>
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
    const defaultGroupId = defaultGroupStr ? parseInt(defaultGroupStr, 10) : null;

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
// Zakladka: NVIDIA NGC
// ==========================================================================

function renderNgcTab() {
  const placeholderChip = `<tf-chip status="info" id="ngc-status-chip">…</tf-chip>`;
  return `
    <div class="card">
      <div class="card-header">
        <h3>${escapeHtml(I18n.t('settings.ngc_title'))} ${placeholderChip}</h3>
      </div>
      <div class="card-body">
        <p class="form-hint" style="margin:0 0 16px;">${escapeHtml(I18n.t('settings.ngc_hint'))}</p>
        <div class="form-row">
          <tf-input id="ngc-key" type="password" label="${escapeAttr(I18n.t('settings.ngc_key_label'))}" placeholder="nvapi-..."></tf-input>
        </div>
        <div style="display:flex;gap:8px;margin-top:16px;">
          <tf-button variant="primary" icon="check" id="ngc-save">${escapeHtml(I18n.t('common.save'))}</tf-button>
          <tf-button variant="ghost" icon="refresh" id="ngc-test">${escapeHtml(I18n.t('settings.ngc_test'))}</tf-button>
        </div>
      </div>
    </div>
  `;
}

function bindNgcTab() {
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

  byId('ngc-save')?.addEventListener('click', async () => {
    const value = byId('ngc-key')?.value?.trim() || '';
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
}

export default SettingsScreen;
