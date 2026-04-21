// =============================================================================
// Plik: modules/my-accounts.js
// Opis: Ekran "Moje polaczone konta" (widok user-a). Grid kart per (addon,
//       provider) w trybie individual. Karty maja trzy stany: active / expired
//       / not_connected. Zrodlo danych: MyOAuthAccountsListRequest.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import { runOAuthPopup } from '/js/modules/addons/oauth-popup.js';

let entries = [];

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

// Gradient kolorystyczny dla ikony addona (deterministyczny per addon_id).
function iconGradient(addonId) {
  const palettes = [
    ['#6366f1', '#8b5cf6'],
    ['#06b6d4', '#22d3ee'],
    ['#ea4335', '#fbbc04'],
    ['#22c55e', '#4ade80'],
    ['#f59e0b', '#f97316'],
    ['#ec4899', '#f472b6'],
    ['#64748b', '#94a3b8'],
  ];
  let h = 0;
  for (let i = 0; i < addonId.length; i++) h = (h * 31 + addonId.charCodeAt(i)) >>> 0;
  const [a, b] = palettes[h % palettes.length];
  return `background:linear-gradient(135deg,${a},${b});`;
}

function initials(text) {
  const raw = (text || '').trim();
  if (!raw) return '?';
  const m = raw.match(/([A-Za-z])/g) || [];
  if (m.length >= 2) return (m[0] + m[1]).toUpperCase();
  return raw.slice(0, 2).toUpperCase();
}

function relativeLastUsed(epoch, nowEpoch) {
  if (!epoch || epoch <= 0) return '';
  const delta = Math.max(0, nowEpoch - Number(epoch));
  if (delta < 60) return I18n.t('my_accounts.last_used_relative_just_now');
  if (delta < 3600) return I18n.t('my_accounts.last_used_relative_min_ago', { n: Math.floor(delta / 60) });
  if (delta < 86400) return I18n.t('my_accounts.last_used_relative_hours_ago', { n: Math.floor(delta / 3600) });
  return I18n.t('my_accounts.last_used_relative_days_ago', { n: Math.floor(delta / 86400) });
}

function fmtShortDate(epoch) {
  if (!epoch || epoch <= 0) return '';
  try {
    return new Date(Number(epoch) * 1000).toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
  } catch {
    return '';
  }
}

function relativeExpired(expiresEpoch, nowEpoch) {
  const delta = Math.max(0, nowEpoch - Number(expiresEpoch));
  if (delta < 3600) return I18n.t('my_accounts.last_used_relative_min_ago', { n: Math.floor(delta / 60) });
  if (delta < 86400) return I18n.t('my_accounts.last_used_relative_hours_ago', { n: Math.floor(delta / 3600) });
  return I18n.t('my_accounts.last_used_relative_days_ago', { n: Math.floor(delta / 86400) });
}

const MyAccountsScreen = {
  get title() {
    return I18n.t('my_accounts.page_title');
  },

  render() {
    return `
      <div class="myaccounts-page">
        <div class="page-header">
          <div>
            <h1>${sprite('link')} ${escapeHtml(I18n.t('my_accounts.page_title'))}</h1>
            <div class="subtitle" id="myacc-sub"></div>
          </div>
          <div class="actions">
            <tf-button variant="secondary" data-role="refresh-all">
              ${sprite('refresh')} ${escapeHtml(I18n.t('my_accounts.refresh_all'))}
            </tf-button>
          </div>
        </div>

        <div class="alert info">
          ${sprite('info')}
          <div>${I18n.t('my_accounts.alert_explainer')}</div>
        </div>

        <div id="myacc-grid" class="myapps-grid"></div>
      </div>
    `;
  },

  async mount() {
    byId('myacc-grid')
      ?.closest('.myaccounts-page')
      ?.querySelector('[data-role="refresh-all"]')
      ?.addEventListener('click', () => onRefreshAll());
    await loadAll();
  },

  unmount() {
    entries = [];
  },
};

async function loadAll() {
  try {
    const rows = await ApiBinary.list('myOAuthAccountsListRequest', { arrayKey: 'accounts' });
    entries = rows.map(normalize);
    renderGrid();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// Pulls the latest list and replaces a single (addonId, providerId) entry
// so we can re-render only its card. Falls back to full reload if the
// entry can't be found after the server-side change.
async function reloadEntry(addonId, providerId) {
  try {
    const rows = await ApiBinary.list('myOAuthAccountsListRequest', { arrayKey: 'accounts' });
    const fresh = rows.map(normalize)
      .find((e) => e.addonId === addonId && e.providerId === providerId);
    if (!fresh) {
      entries = rows.map(normalize);
      renderGrid();
      return;
    }
    const idx = entries.findIndex((e) => e.addonId === addonId && e.providerId === providerId);
    if (idx >= 0) entries[idx] = fresh;
    else entries.push(fresh);
    patchCard(fresh);
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// Replaces a single card's DOM in place without touching the grid.
function patchCard(entry) {
  const grid = byId('myacc-grid');
  if (!grid) return;
  const card = grid.querySelector(
    `.myapp-card[data-addon="${CSS.escape(entry.addonId)}"][data-provider="${CSS.escape(entry.providerId)}"]`,
  );
  if (!card) {
    renderGrid();
    return;
  }
  const nowEpoch = Math.floor(Date.now() / 1000);
  const tpl = document.createElement('div');
  tpl.innerHTML = renderCard(entry, nowEpoch).trim();
  const fresh = tpl.firstElementChild;
  if (!fresh) return;
  card.replaceWith(fresh);
  wireCardActions(grid);
  // Keep the header subtitle count in sync.
  const sub = byId('myacc-sub');
  const active = entries.filter((e) => e.status === 'active').length;
  if (sub) sub.textContent = I18n.t('my_accounts.subtitle', { n: active });
}

function normalize(a) {
  return {
    addonId: a.addonId ?? a.addon_id,
    addonName: a.addonName ?? a.addon_name,
    addonIcon: a.addonIcon ?? a.addon_icon ?? null,
    addonDescription: a.addonDescription ?? a.addon_description ?? '',
    addonVersion: a.addonVersion ?? a.addon_version ?? '',
    providerId: a.providerId ?? a.provider_id,
    providerDisplayName: a.providerDisplayName ?? a.provider_display_name ?? '',
    status: a.status || 'not_connected',
    accountId: a.accountId ?? a.account_id ?? null,
    accountEmail: a.accountEmail ?? a.account_email ?? '',
    accountDisplayName: a.accountDisplayName ?? a.account_display_name ?? '',
    scopes: Array.isArray(a.scopes) ? a.scopes : [],
    connectedAtEpoch: Number(a.connectedAtEpoch ?? a.connected_at_epoch ?? 0),
    lastUsedAtEpoch: Number(a.lastUsedAtEpoch ?? a.last_used_at_epoch ?? 0),
    expiresAtEpoch: Number(a.expiresAtEpoch ?? a.expires_at_epoch ?? 0),
  };
}

function renderGrid() {
  const grid = byId('myacc-grid');
  const sub = byId('myacc-sub');
  if (!grid) return;
  const active = entries.filter((e) => e.status === 'active').length;
  if (sub) sub.textContent = I18n.t('my_accounts.subtitle', { n: active });
  if (entries.length === 0) {
    grid.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('my_accounts.empty_state'))}</div>`;
    return;
  }
  const nowEpoch = Math.floor(Date.now() / 1000);
  grid.innerHTML = entries.map((e) => renderCard(e, nowEpoch)).join('');
  wireCardActions(grid);
}

function statusChip(status) {
  const label = I18n.t(`my_accounts.status_${status}`);
  if (status === 'active') return `<tf-chip status="ok" dot>${escapeHtml(label)}</tf-chip>`;
  if (status === 'expired') return `<tf-chip status="warn" dot>${escapeHtml(label)}</tf-chip>`;
  if (status === 'revoked') return `<tf-chip status="err" dot>${escapeHtml(label)}</tf-chip>`;
  return `<tf-chip status="info">${escapeHtml(label)}</tf-chip>`;
}

function renderCard(e, nowEpoch) {
  const iconId = e.addonIcon || 'puzzle';
  return `
    <div class="myapp-card" data-addon="${escapeHtml(e.addonId)}" data-provider="${escapeHtml(e.providerId)}">
      <div class="myapp-head">
        <div class="myapp-ico" style="${iconGradient(e.addonId)}">${sprite(iconId)}</div>
        <div class="myapp-meta">
          <div class="myapp-name">
            ${escapeHtml(e.addonName || e.addonId)}
            ${e.addonVersion ? `<tf-chip status="info">v${escapeHtml(e.addonVersion)}</tf-chip>` : ''}
          </div>
          <div class="myapp-desc">${escapeHtml(e.addonDescription || e.providerDisplayName || '')}</div>
        </div>
        ${statusChip(e.status)}
      </div>
      ${renderBody(e, nowEpoch)}
    </div>
  `;
}

function renderBody(e, nowEpoch) {
  if (e.status === 'not_connected') {
    const provider = e.providerDisplayName || e.providerId;
    return `
      <div class="myapp-unlinked">
        <div class="muted-text">${escapeHtml(I18n.t('my_accounts.not_connected_hint', { provider }))}</div>
        <tf-button variant="primary" data-role="connect">
          ${sprite('link')} ${escapeHtml(I18n.t('my_accounts.connect_button', { provider }))}
        </tf-button>
      </div>
    `;
  }
  const email = e.accountEmail || e.accountDisplayName || e.providerId;
  const expiredClass = e.status === 'expired' ? ' expired' : '';
  let metaLine = '';
  if (e.status === 'expired') {
    const when = relativeExpired(e.expiresAtEpoch, nowEpoch);
    metaLine = escapeHtml(I18n.t('my_accounts.token_expired_hint', { when }));
  } else {
    const parts = [];
    if (e.connectedAtEpoch > 0) {
      parts.push(escapeHtml(I18n.t('my_accounts.connected_at', { when: fmtShortDate(e.connectedAtEpoch) })));
    }
    const used = relativeLastUsed(e.lastUsedAtEpoch, nowEpoch);
    if (used) parts.push(escapeHtml(used));
    if (e.scopes.length > 0) {
      const shown = e.scopes.slice(0, 2).join(', ');
      const extra = e.scopes.length > 2 ? ` +${e.scopes.length - 2}` : '';
      parts.push(`${escapeHtml(I18n.t('my_accounts.scopes_label'))}: ${escapeHtml(shown)}${extra}`);
    }
    metaLine = parts.join(' · ');
  }
  // Rola reauth/connect dzieli sie wizualnie: expired = primary "Re-authorize",
  // active = brak przycisku refresh (serwer nie udostepnia handler-a refresh-now).
  const primaryAction = e.status === 'expired'
    ? `<tf-button variant="primary" size="sm" data-role="reauth">${sprite('link')} ${escapeHtml(I18n.t('my_accounts.reauthorize'))}</tf-button>`
    : '';
  return `
    <div class="myapp-linked${expiredClass}">
      <div class="linked-avatar">${escapeHtml(initials(email))}</div>
      <div class="linked-info">
        <div class="linked-email">${escapeHtml(email)}</div>
        <div class="linked-meta">${metaLine}</div>
      </div>
      <div class="linked-actions">
        ${primaryAction}
        <tf-button variant="ghost" size="sm" data-role="disconnect" title="${escapeHtml(I18n.t('my_accounts.disconnect'))}">
          ${sprite('unlink')}
        </tf-button>
      </div>
    </div>
  `;
}

function wireCardActions(grid) {
  grid.querySelectorAll('.myapp-card').forEach((card) => {
    const addonId = card.dataset.addon;
    const providerId = card.dataset.provider;
    const entry = entries.find((e) => e.addonId === addonId && e.providerId === providerId);
    if (!entry) return;

    card.querySelector('[data-role="connect"]')?.addEventListener('click', () => onConnect(entry));
    card.querySelector('[data-role="reauth"]')?.addEventListener('click', () => onReauth(entry));
    card.querySelector('[data-role="disconnect"]')?.addEventListener('click', () => onDisconnect(entry));
  });
}

async function onConnect(entry) {
  try {
    await runOAuthPopup({
      addon_id: entry.addonId,
      provider_id: entry.providerId,
      mode: 'individual',
    });
    toast(I18n.t('common.saved'), 'success');
    await reloadEntry(entry.addonId, entry.providerId);
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

async function onReauth(entry) {
  if (!entry.accountId) return;
  try {
    await runOAuthPopup({ accountIdForReauth: entry.accountId });
    toast(I18n.t('common.saved'), 'success');
    await reloadEntry(entry.addonId, entry.providerId);
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

async function onDisconnect(entry) {
  if (!entry.accountId) return;
  const ok = await TfWindow.confirm({
    title: I18n.t('my_accounts.disconnect_confirm_title'),
    message: I18n.t('my_accounts.disconnect_confirm_body', { email: entry.accountEmail || entry.accountDisplayName || '' }),
    confirmLabel: I18n.t('my_accounts.disconnect'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await ApiBinary.action('addonOAuthRevokeRequest', { accountId: entry.accountId });
    entry.status = 'not_connected';
    entry.accountId = null;
    entry.accountEmail = '';
    entry.accountDisplayName = '';
    entry.scopes = [];
    entry.connectedAtEpoch = 0;
    entry.lastUsedAtEpoch = 0;
    entry.expiresAtEpoch = 0;
    patchCard(entry);
    toast(I18n.t('common.saved'), 'success');
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// Re-authorize dla wszystkich wygasajacych/wygasnietych jednym klikiem.
// Serwer nie udostepnia standalone refresh-now, wiec otwieramy kolejne popupy
// tylko dla kont expired — dla active pozostawiamy bez zmian.
async function onRefreshAll() {
  const toReauth = entries.filter((e) => e.status === 'expired' && e.accountId);
  if (toReauth.length === 0) {
    toast(I18n.t('my_accounts.refresh_success', { n: 0 }), 'success');
    return;
  }
  let done = 0;
  for (const e of toReauth) {
    try {
      await runOAuthPopup({ accountIdForReauth: e.accountId });
      done += 1;
    } catch {
      // Pomijamy — user moze anulowac popup; kontynuujemy petle.
    }
  }
  await loadAll();
  toast(I18n.t('my_accounts.refresh_success', { n: done }), 'success');
}

export default MyAccountsScreen;
