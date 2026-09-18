// =============================================================================
// Plik: modules/my-accounts.js
// Opis: Ekran "Moje polaczone konta" (widok user-a). Dwie sekcje kart:
//       dodatki per (addon, provider) w trybie individual
//       (MyOAuthAccountsListRequest) oraz aplikacje agentowe per silnik CLI
//       (U01, ProviderAccountBody: MyAccountList + AccountList po katalog
//       silnikow). Karty dodatkow maja stany active / expired / not_connected,
//       karty aplikacji — wlasne konto uzytkownika i konta firmowe mu nadane.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeAttr, escapeHtml, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import { runOAuthPopup } from '/js/modules/addons/oauth-popup.js';
import {
  AgentAccounts,
  T,
  accountSubtitle,
  credentialKindLabel,
  engineName,
  engineTile,
  errorText,
  statusChipHtml,
  usedOnLabel,
  whenLabel,
} from '/js/modules/agent-accounts.js';
import { openCreateAccountWindow } from '/js/modules/agent-accounts-window.js';
import { openLoginWizard } from '/js/modules/agent-accounts-login.js';

let entries = [];
// U01 — the caller's agent applications: the engine catalog and the accounts
// they may use, one card per engine.
let agentEngines = [];
let agentAccounts = [];

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

        <h4 class="aa-sub-h">${escapeHtml(I18n.t('my_accounts.section_addons'))}</h4>
        <div id="myacc-grid" class="myapps-grid"></div>

        <h4 class="aa-sub-h">${escapeHtml(T('apps_section'))}</h4>
        <div id="myacc-apps" class="myapps-grid"></div>
      </div>
    `;
  },

  async mount() {
    byId('myacc-grid')
      ?.closest('.myaccounts-page')
      ?.querySelector('[data-role="refresh-all"]')
      ?.addEventListener('click', () => onRefreshAll());
    await Promise.all([loadAll(), loadAgentApps()]);
  },

  unmount() {
    entries = [];
    agentEngines = [];
    agentAccounts = [];
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

// =============================================================================
// U01 — Aplikacje agentowe
// =============================================================================

// The engine catalog comes from the admin list request, which answers any
// caller (a non-administrator simply gets their own and granted accounts back);
// the accounts themselves come from the caller's own list, which is the one
// that says whether they may delete each of them.
async function loadAgentApps() {
  try {
    const [catalog, mine] = await Promise.all([
      AgentAccounts.list({}),
      AgentAccounts.mine(),
    ]);
    agentEngines = catalog?.engines ?? [];
    agentAccounts = mine?.accounts ?? [];
  } catch (err) {
    agentEngines = [];
    agentAccounts = [];
    toast(errorText(err), 'error');
  }
  renderAgentApps();
}

function renderAgentApps() {
  const grid = byId('myacc-apps');
  if (!grid) return;
  if (!agentEngines.length) {
    grid.innerHTML = `<div class="aa-apps-empty">${escapeHtml(T('apps_empty'))}</div>`;
    return;
  }
  grid.innerHTML = agentEngines
    .map((engine) => renderAgentCard(engine.engine_id ?? engine.engineId))
    .join('');
  wireAgentCards(grid);
}

function renderAgentCard(engineId) {
  const forEngine = agentAccounts.filter((a) => (a.engine_id ?? a.engineId) === engineId);
  const own = forEngine.find((a) => a.scope === 'user') ?? null;
  const shared = forEngine.filter((a) => a.scope !== 'user');
  const head = own
    ? statusChipHtml(own)
    : `<tf-chip status="info" label="${escapeAttr(T('apps_not_connected'))}"></tf-chip>`;
  return `
    <div class="myapp-card aa-app-card" data-engine="${escapeHtml(engineId)}">
      <div class="myapp-head">
        ${engineTile(engineId)}
        <div class="myapp-meta">
          <div class="myapp-name">${escapeHtml(engineName(engineId, agentEngines))}</div>
          <div class="myapp-desc">${escapeHtml(T('apps_card_sub'))}</div>
        </div>
        ${head}
      </div>
      ${own ? renderOwnAccount(own) : renderConnectPrompt()}
      ${shared.map(renderSharedAccount).join('')}
    </div>
  `;
}

function renderOwnAccount(account) {
  // The account's NAME leads, the way the mockup's connected card does; the
  // provider identity, where it is in use and how it authenticates are the
  // lines under it.
  const title = account.display_name || accountSubtitle(account) || '';
  const lastUsed = account.last_used_at ?? account.lastUsedAt;
  const used = lastUsed ? T('apps_last_used', { when: whenLabel(lastUsed) }) : T('apps_never_used');
  const kind = credentialKindLabel(account.credential_kind ?? account.credentialKind);
  const meta = [accountSubtitle(account), kind, used].filter(Boolean).join(' · ');
  // "Używane na" and the session count are what THIS node measured; a session
  // running on a peer is in neither, which is why the line says "tu".
  const sessions = Number(account.session_count ?? account.sessionCount ?? 0);
  const usage = [
    T('apps_used_on', { nodes: usedOnLabel(account) }),
    sessions > 0 ? T('apps_sessions', { count: sessions }) : '',
  ].filter(Boolean).join(' · ');
  const canDelete = (account.can_delete ?? account.canDelete) !== false;
  const canLogin = (account.can_login ?? account.canLogin) === true;
  // U01 carries a status, not a credential revision: an account that is active
  // holds one, and anything else is still waiting for a first sign-in.
  const signedIn = account.status === 'active';
  return `
    <div class="myapp-linked">
      <div class="linked-avatar">${escapeHtml(initials(title))}</div>
      <div class="linked-info">
        <div class="linked-email">${escapeHtml(title)}</div>
        <div class="linked-meta">${escapeHtml(meta)}</div>
        <div class="linked-meta">${escapeHtml(usage)}</div>
      </div>
      <div class="linked-actions">
        ${canLogin ? `<tf-button variant="secondary" size="sm" data-role="app-login">${escapeHtml(T(signedIn ? 'action_relogin' : 'action_login'))}</tf-button>` : ''}
        ${canDelete ? `<tf-button variant="ghost" size="sm" data-role="app-disconnect">${escapeHtml(T('apps_disconnect'))}</tf-button>` : ''}
      </div>
    </div>
  `;
}

// A shared account is somebody else's to manage: it is listed so the user knows
// which company account their agents may use, with no action they could take.
function renderSharedAccount(account) {
  const subtitle = accountSubtitle(account);
  return `
    <div class="myapp-linked">
      <div class="linked-avatar">${escapeHtml(initials(account.display_name ?? ''))}</div>
      <div class="linked-info">
        <div class="linked-email">${escapeHtml(account.display_name ?? '')}</div>
        <div class="linked-meta">${escapeHtml([T('apps_shared'), subtitle].filter(Boolean).join(' · '))}</div>
      </div>
      <div class="linked-actions">${statusChipHtml(account)}</div>
    </div>
  `;
}

function renderConnectPrompt() {
  return `
    <div class="myapp-unlinked">
      <div class="muted-text">${escapeHtml(T('apps_connect_hint'))}</div>
      <tf-button variant="primary" size="sm" data-role="app-connect">${escapeHtml(T('apps_connect'))}</tf-button>
    </div>
  `;
}

/** The caller's OWN account for one engine, which is the only one they manage. */
function ownAccount(engineId) {
  return agentAccounts.find(
    (a) => (a.engine_id ?? a.engineId) === engineId && a.scope === 'user',
  ) ?? null;
}

// A02 for a personal account. The node is Core's to pick: reading the runtime
// matrix is an administrator's right, so a user signing their own account in
// names no node and the account's home (or this one) runs the terminal.
function openOwnLogin(account, engineId) {
  openLoginWizard({
    accountId: account.account_id ?? account.accountId,
    accountName: account.display_name ?? account.displayName ?? '',
    engineId,
    engines: agentEngines,
    onFinished: () => loadAgentApps(),
  });
}

function wireAgentCards(grid) {
  grid.querySelectorAll('.myapp-card[data-engine]').forEach((card) => {
    const engineId = card.dataset.engine;
    card.querySelector('[data-role="app-connect"]')?.addEventListener('click', () => {
      const engine = agentEngines.find((e) => (e.engine_id ?? e.engineId) === engineId);
      openCreateAccountWindow({
        engines: engine ? [engine] : agentEngines,
        scope: 'user',
        // Connecting is one step in the person's head: the account is created
        // and the sign-in it exists for opens on top of it.
        onCreated: () => loadAgentApps().then(() => {
          const account = ownAccount(engineId);
          if (account && (account.can_login ?? account.canLogin) === true) {
            openOwnLogin(account, engineId);
          }
        }),
      });
    });
    card.querySelector('[data-role="app-login"]')?.addEventListener('click', () => {
      const account = ownAccount(engineId);
      if (account) openOwnLogin(account, engineId);
    });
    card.querySelector('[data-role="app-disconnect"]')?.addEventListener('click', async () => {
      const account = ownAccount(engineId);
      if (!account) return;
      const ok = await TfWindow.confirm({
        title: T('apps_disconnect_confirm_title'),
        message: T('apps_disconnect_confirm_body', { name: escapeHtml(account.display_name ?? '') }),
        confirmLabel: T('apps_disconnect'),
        cancelLabel: I18n.t('common.cancel'),
        danger: true,
      });
      if (!ok) return;
      try {
        await AgentAccounts.remove(account.account_id ?? account.accountId);
        toast(T('apps_disconnected'), 'success');
        await loadAgentApps();
      } catch (err) {
        toast(errorText(err), 'error');
      }
    });
  });
}

export default MyAccountsScreen;
