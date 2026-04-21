// =============================================================================
// Plik: modules/addons/oauth-config.js
// Opis: Tab OAuth Config dla detail addona (admin). Dla kazdego providera
//       z manifestu renderuje trzy sekcje: tryb OAuth (global/individual +
//       secondary disable toggle), dane providera (client_id/secret/tenant/
//       scopes/redirect_uri + save/test/clear/connect-shared) oraz info-card.
// Przyklad: OAuthConfigTab.mount(container, addonId, { providerDecls }).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { runOAuthPopup } from '/js/modules/addons/oauth-popup.js';

let currentAddonId = null;
let providers = [];
let configs = new Map();
// Lokalny stan trybu OAuth per provider — zapisywany dopiero po kliknieciu "Save".
let pendingModes = new Map();

export const OAuthConfigTab = {
  async mount(container, addonId, { providerDecls = [] } = {}) {
    currentAddonId = addonId;
    providers = providerDecls.map((p) => ({
      providerId: p.providerId ?? p.provider_id,
      displayName: p.displayName ?? p.display_name,
      authorizeUrl: p.authorizeUrl ?? p.authorize_url,
      tokenUrl: p.tokenUrl ?? p.token_url,
      scopes: p.scopes || [],
      mode: p.mode || 'individual',
      pkce: !!p.pkce,
    }));
    await loadConfigs();
    pendingModes = new Map();
    for (const p of providers) {
      const cfg = configs.get(p.providerId);
      pendingModes.set(p.providerId, (cfg && cfg.oauthMode) || p.mode || 'individual');
    }
    render(container);
  },

  unmount() {
    currentAddonId = null;
    providers = [];
    configs = new Map();
    pendingModes = new Map();
  },
};

function buildConfig(c) {
  const pid = c.providerId ?? c.provider_id;
  return {
    providerId: pid,
    clientId: c.clientId ?? c.client_id ?? '',
    clientSecretSet: !!(c.clientSecretSet ?? c.client_secret_set),
    redirectUri: c.redirectUri ?? c.redirect_uri ?? '',
    enabled: !!c.enabled,
    oauthMode: c.oauthMode ?? c.oauth_mode ?? null,
    linkedAccountsCount: c.linkedAccountsCount ?? c.linked_accounts_count ?? 0,
    sharedAccountEmail: c.sharedAccountEmail ?? c.shared_account_email ?? null,
  };
}

async function loadConfigs() {
  try {
    const resp = await ApiBinary.one('addonOAuthConfigListRequest', { addonId: currentAddonId });
    configs = new Map();
    for (const c of (resp.configs || [])) {
      const cfg = buildConfig(c);
      configs.set(cfg.providerId, cfg);
    }
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// Refetches only the config for one provider and updates the map in place.
async function loadConfigForProvider(providerId) {
  try {
    const resp = await ApiBinary.one('addonOAuthConfigListRequest', { addonId: currentAddonId });
    for (const c of (resp.configs || [])) {
      const cfg = buildConfig(c);
      if (cfg.providerId === providerId) {
        configs.set(providerId, cfg);
        return;
      }
    }
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// Re-renders one provider block in place, preserving sibling blocks and the
// info alert at the top of the tab. Rewires handlers for the new DOM.
function rerenderProviderBlock(container, p) {
  const block = container.querySelector(`.oauth-provider-block[data-provider="${CSS.escape(p.providerId)}"]`);
  if (!block) return;
  const tpl = document.createElement('div');
  tpl.innerHTML = renderProviderBlock(p).trim();
  const fresh = tpl.firstElementChild;
  if (!fresh) return;
  block.replaceWith(fresh);
  attachProviderHandlers(container, p);
}

function defaultRedirect() {
  return `${window.location.origin}/oauth/addon/callback`;
}

function render(container) {
  if (providers.length === 0) {
    container.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('addon_oauth.no_providers'))}</div>`;
    return;
  }
  container.innerHTML = `
    <div class="alert info" style="margin-bottom:14px;">
      <svg class="icon" width="18" height="18" style="flex-shrink:0;"><use href="#i-info"/></svg>
      <div>${escapeHtml(I18n.t('addon_oauth.explainer'))}</div>
    </div>
    ${providers.map((p) => renderProviderBlock(p)).join('')}
  `;
  for (const p of providers) attachProviderHandlers(container, p);
}

function renderProviderBlock(p) {
  const cfg = configs.get(p.providerId) || {
    clientId: '',
    clientSecretSet: false,
    redirectUri: defaultRedirect(),
    enabled: false,
    oauthMode: null,
    linkedAccountsCount: 0,
    sharedAccountEmail: null,
  };
  const currentMode = pendingModes.get(p.providerId) || 'individual';
  const isMs = p.providerId === 'microsoft' || /microsoft|azure/i.test(p.displayName || '');
  return `
    <div class="oauth-provider-block" data-provider="${escapeAttr(p.providerId)}">
      ${renderModeSection(p, cfg, currentMode)}
      ${renderCredentialsSection(p, cfg, currentMode, isMs)}
      ${renderInfoSection(currentMode)}
    </div>
  `;
}

function renderModeSection(p, cfg, currentMode) {
  const n = cfg.linkedAccountsCount || 0;
  const warning = I18n.t('addon_oauth.mode_switch_warning', { n }).replace('{n}', String(n));
  return `
    <div class="section-card">
      <h3>${escapeHtml(I18n.t('addon_oauth.mode_title'))}</h3>
      <div class="section-sub">${escapeHtml(I18n.t('addon_oauth.mode_subtitle'))}</div>

      <div class="oauth-mode-grid">
        <div class="oauth-mode-card global ${currentMode === 'global' ? 'active' : ''}"
             data-mode="global" role="radio" aria-checked="${currentMode === 'global'}" tabindex="0">
          <div class="mode-radio"></div>
          <div class="mode-icon"><svg width="22" height="22"><use href="#i-globe"/></svg></div>
          <h4>${escapeHtml(I18n.t('addon_oauth.mode_global_title'))}</h4>
          <p>${escapeHtml(I18n.t('addon_oauth.mode_global_desc'))}</p>
          <div class="mode-hint">${escapeHtml(I18n.t('addon_oauth.mode_global_hint'))}</div>
        </div>
        <div class="oauth-mode-card individual ${currentMode === 'individual' ? 'active' : ''}"
             data-mode="individual" role="radio" aria-checked="${currentMode === 'individual'}" tabindex="0">
          <div class="mode-radio"></div>
          <div class="mode-icon"><svg width="22" height="22"><use href="#i-user"/></svg></div>
          <h4>${escapeHtml(I18n.t('addon_oauth.mode_individual_title'))}</h4>
          <p>${escapeHtml(I18n.t('addon_oauth.mode_individual_desc'))}</p>
          <div class="mode-hint">${escapeHtml(I18n.t('addon_oauth.mode_individual_hint'))}</div>
        </div>
      </div>

      <div class="oauth-mode-disable ${currentMode === 'none' ? 'active' : ''}" data-role="disable-row">
        <tf-toggle data-role="disable-oauth" ${currentMode === 'none' ? 'checked' : ''}></tf-toggle>
        <label>${escapeHtml(I18n.t('addon_oauth.disable_oauth'))}</label>
      </div>

      <div class="alert warn" style="margin-top:12px;">
        <svg class="icon" width="18" height="18" style="flex-shrink:0;"><use href="#i-alert"/></svg>
        <div>${escapeHtml(warning)}</div>
      </div>
    </div>
  `;
}

function renderCredentialsSection(p, cfg, currentMode, isMs) {
  const secretPlaceholder = cfg.clientSecretSet
    ? I18n.t('addon_oauth.client_secret_set_placeholder')
    : '';
  return `
    <div class="section-card">
      <h3>${escapeHtml(I18n.t('addon_oauth.credentials_title'))}</h3>
      <div class="section-sub">${escapeHtml(I18n.t('addon_oauth.credentials_subtitle'))}</div>

      <div class="form-row">
        <label>${escapeHtml(I18n.t('addon_oauth.provider_label'))}
          <div class="label-desc">${escapeHtml(I18n.t('addon_oauth.provider_desc'))}</div>
        </label>
        <tf-select disabled value="${escapeAttr(p.providerId)}" data-role="provider-id">
          <option value="${escapeAttr(p.providerId)}">${escapeHtml(p.displayName || p.providerId)}</option>
        </tf-select>
      </div>

      <div class="form-row">
        <label>${escapeHtml(I18n.t('addon_oauth.client_id'))}
          <div class="label-desc">${escapeHtml(I18n.t('addon_oauth.client_id_desc'))}</div>
        </label>
        <tf-input class="mono" value="${escapeAttr(cfg.clientId)}" data-role="client-id" placeholder="client_id"></tf-input>
      </div>

      <div class="form-row">
        <label>${escapeHtml(I18n.t('addon_oauth.client_secret'))}
          <div class="label-desc">${escapeHtml(I18n.t('addon_oauth.client_secret_desc'))}</div>
        </label>
        <div class="secret-row">
          <tf-input type="password" placeholder="${escapeAttr(secretPlaceholder)}" data-role="client-secret"></tf-input>
          <tf-button variant="secondary" size="sm" data-role="toggle-secret" title="${escapeAttr(I18n.t('addon_oauth.toggle_secret'))}">
            <svg width="14" height="14"><use href="#i-search"/></svg>
          </tf-button>
          <tf-button variant="secondary" size="sm" data-role="change-secret">
            <svg width="14" height="14"><use href="#i-settings"/></svg>
            ${escapeHtml(I18n.t('common.change'))}
          </tf-button>
        </div>
      </div>

      ${isMs ? `
        <div class="form-row">
          <label>${escapeHtml(I18n.t('addon_oauth.tenant_id'))}
            <div class="label-desc">${escapeHtml(I18n.t('addon_oauth.tenant_id_desc'))}</div>
          </label>
          <tf-input class="mono" value="common" data-role="tenant-id" placeholder="common"></tf-input>
        </div>
      ` : ''}

      <div class="form-row">
        <label>${escapeHtml(I18n.t('addon_oauth.scopes_label'))}
          <div class="label-desc">${escapeHtml(I18n.t('addon_oauth.scopes_desc'))}</div>
        </label>
        <div class="scopes-chips">
          ${(p.scopes || []).map((s) => `<span class="oauth-scope-chip">${escapeHtml(s)}</span>`).join('')}
        </div>
      </div>

      <div class="form-row">
        <label>${escapeHtml(I18n.t('addon_oauth.redirect_uri'))}
          <div class="label-desc">${escapeHtml(I18n.t('addon_oauth.redirect_uri_desc'))}</div>
        </label>
        <tf-input class="mono" value="${escapeAttr(cfg.redirectUri || defaultRedirect())}" data-role="redirect-uri" readonly></tf-input>
      </div>

      ${currentMode === 'global' && cfg.sharedAccountEmail ? `
        <div class="shared-account-indicator">
          <svg width="16" height="16"><use href="#i-check"/></svg>
          <span>${escapeHtml(I18n.t('addon_oauth.shared_connected_as', { email: cfg.sharedAccountEmail }).replace('{email}', cfg.sharedAccountEmail))}</span>
        </div>
      ` : ''}

      <div class="oauth-actions">
        <tf-button variant="primary" data-role="save">
          <svg width="14" height="14"><use href="#i-check"/></svg>
          ${escapeHtml(I18n.t('addon_oauth.save'))}
        </tf-button>
        <tf-button variant="secondary" data-role="test-connection">
          <svg width="14" height="14"><use href="#i-arrow-out"/></svg>
          ${escapeHtml(I18n.t('addon_oauth.test_connection'))}
        </tf-button>
        ${currentMode === 'global' && !cfg.sharedAccountEmail ? `
          <tf-button variant="secondary" data-role="connect-shared">
            <svg width="14" height="14"><use href="#i-share"/></svg>
            ${escapeHtml(I18n.t('addon_oauth.connect_shared'))}
          </tf-button>
        ` : ''}
        <tf-button variant="ghost" data-role="clear-secret" class="danger" ${cfg.clientSecretSet ? '' : 'disabled'}>
          ${escapeHtml(I18n.t('addon_oauth.clear_secret'))}
        </tf-button>
      </div>
    </div>
  `;
}

function renderInfoSection(currentMode) {
  const key = currentMode === 'global'
    ? 'addon_oauth.info_global'
    : currentMode === 'none'
      ? 'addon_oauth.info_disabled'
      : 'addon_oauth.info_individual';
  return `
    <div class="section-card">
      <h3>${escapeHtml(I18n.t('addon_oauth.info_title'))}</h3>
      <p class="info-paragraph">${escapeHtml(I18n.t(key))}</p>
    </div>
  `;
}

function attachProviderHandlers(container, p) {
  const block = container.querySelector(`.oauth-provider-block[data-provider="${CSS.escape(p.providerId)}"]`);
  if (!block) return;

  const setMode = (mode) => {
    pendingModes.set(p.providerId, mode);
    // Re-render tylko tego bloku — stan lokalny (pendingModes) juz uaktualniony.
    block.outerHTML = renderProviderBlock(p);
    const newBlock = container.querySelector(`.oauth-provider-block[data-provider="${CSS.escape(p.providerId)}"]`);
    if (newBlock) attachProviderHandlers(container, p);
  };

  block.querySelectorAll('.oauth-mode-card').forEach((card) => {
    card.addEventListener('click', () => setMode(card.dataset.mode));
    card.addEventListener('keydown', (e) => {
      if (e.key === ' ' || e.key === 'Enter') {
        e.preventDefault();
        setMode(card.dataset.mode);
      }
    });
  });

  const disableToggle = block.querySelector('tf-toggle[data-role="disable-oauth"]');
  disableToggle?.addEventListener('change', (e) => {
    const checked = !!e.detail?.checked;
    setMode(checked ? 'none' : 'individual');
  });

  // Eye toggle na password input — ukryj/pokaz secret plaintext.
  block.querySelector('[data-role="toggle-secret"]')?.addEventListener('click', () => {
    const input = block.querySelector('[data-role="client-secret"]');
    if (!input) return;
    const cur = input.getAttribute('type') || 'password';
    input.setAttribute('type', cur === 'password' ? 'text' : 'password');
  });

  // Change secret — wyczysc pole (user wpisze nowy; backend wykryje ze jest non-empty).
  block.querySelector('[data-role="change-secret"]')?.addEventListener('click', () => {
    const input = block.querySelector('[data-role="client-secret"]');
    if (input) {
      input.value = '';
      input.setAttribute('type', 'text');
      input.focus?.();
    }
  });

  block.querySelector('[data-role="save"]')?.addEventListener('click', async () => {
    const clientId = block.querySelector('[data-role="client-id"]')?.value || '';
    const secretEl = block.querySelector('[data-role="client-secret"]');
    const clientSecret = secretEl?.value || '';
    const redirectUri = block.querySelector('[data-role="redirect-uri"]')?.value || defaultRedirect();
    const oauthMode = pendingModes.get(p.providerId) || 'individual';
    // Enabled = true gdy mode != none. Admin wylacza OAuth wybierajac tryb "none".
    const enabled = oauthMode !== 'none';
    try {
      await ApiBinary.action('addonOAuthConfigSetRequest', {
        addonId: currentAddonId,
        providerId: p.providerId,
        clientId,
        clientSecret: clientSecret ? clientSecret : null,
        redirectUri,
        enabled,
        oauthMode,
      });
      toast(I18n.t('addon_oauth.saved'), 'success');
      if (secretEl) secretEl.value = '';
      await loadConfigForProvider(p.providerId);
      rerenderProviderBlock(container, p);
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });

  block.querySelector('[data-role="test-connection"]')?.addEventListener('click', async () => {
    try {
      const resp = await ApiBinary.one('addonOAuthTestConnectionRequest', {
        addonId: currentAddonId,
        providerId: p.providerId,
      });
      if (resp.ok) {
        const email = resp.accountEmail ?? resp.account_email ?? '';
        toast(I18n.t('addon_oauth.test_success', { email }).replace('{email}', email), 'success');
      } else {
        const msg = resp.message || '';
        if (msg === 'not_configured' || msg === 'disabled') {
          toast(I18n.t('addon_oauth.test_not_configured'), 'error');
        } else {
          toast(I18n.t('addon_oauth.test_failed', { error: msg }).replace('{error}', msg), 'error');
        }
      }
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });

  block.querySelector('[data-role="clear-secret"]')?.addEventListener('click', async () => {
    if (!window.confirm(I18n.t('addon_oauth.clear_secret_confirm_body'))) return;
    try {
      await ApiBinary.action('addonOAuthConfigClearSecretRequest', {
        addonId: currentAddonId,
        providerId: p.providerId,
      });
      toast(I18n.t('addon_oauth.saved'), 'success');
      await loadConfigForProvider(p.providerId);
      rerenderProviderBlock(container, p);
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });

  block.querySelector('[data-role="connect-shared"]')?.addEventListener('click', async () => {
    try {
      await runOAuthPopup({
        addon_id: currentAddonId,
        provider_id: p.providerId,
        mode: 'global',
      });
      await loadConfigForProvider(p.providerId);
      rerenderProviderBlock(container, p);
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });
}
