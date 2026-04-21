// =============================================================================
// Plik: modules/addons/linked-accounts.js
// Opis: Tab Linked Accounts (admin). Tabela kont OAuth podpietych do addona
//       (scope='all'): user + grupa, email + tenant, connected_at, last_used
//       (z relatywnym czasem), status (active/expired/revoked), liczba scopes,
//       akcje per-row (Revoke / Re-auth). Toolbar z wyszukiwarka i chipami
//       filtra oraz naglowkowa akcja "Revoke all" z confirm dialogiem.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { runOAuthPopup } from '/js/modules/addons/oauth-popup.js';
import { TfWindow } from '/js/components/tf-window.js';

let currentAddonId = null;
let accounts = [];
let filterStatus = 'all';
let filterSearch = '';

export const LinkedAccountsTab = {
  async mount(container, addonId) {
    currentAddonId = addonId;
    accounts = [];
    filterStatus = 'all';
    filterSearch = '';
    await loadAll(container);
  },

  unmount() {
    currentAddonId = null;
    accounts = [];
  },
};

async function loadAll(container) {
  container.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>`;
  try {
    const resp = await ApiBinary.one('addonOAuthLinkedAccountsRequest', {
      addonId: currentAddonId,
      scope: 'all',
    });
    accounts = (resp.accounts || []).map(normalize);
    render(container);
  } catch (err) {
    container.innerHTML = `<div class="addons-empty" style="color:var(--danger);">${escapeHtml(err.message)}</div>`;
  }
}

// Fetches the linked-accounts list and replaces just one entry in place.
// Used after re-auth so the updated row (new expiry / status / lastUsed)
// re-renders without tearing down the table.
async function reloadAccount(accountId) {
  try {
    const resp = await ApiBinary.one('addonOAuthLinkedAccountsRequest', {
      addonId: currentAddonId,
      scope: 'all',
    });
    const updated = (resp.accounts || [])
      .map(normalize)
      .find((a) => a.id === accountId);
    if (!updated) return;
    const idx = accounts.findIndex((a) => a.id === accountId);
    if (idx >= 0) accounts[idx] = updated;
  } catch (_) { /* ignore */ }
}

function normalize(a) {
  const nowEpoch = Math.floor(Date.now() / 1000);
  const expiresAt = a.expiresAtEpoch ?? a.expires_at_epoch ?? null;
  const revoked = !!a.revoked;
  const status = revoked
    ? 'revoked'
    : (expiresAt && expiresAt > 0 && expiresAt < nowEpoch + 60 ? 'expired' : 'active');
  const scopes = a.scopes || [];
  const email = a.externalAccountId ?? a.external_account_id ?? '';
  const displayName = a.displayName ?? a.display_name ?? email;
  return {
    id: a.id,
    userId: a.userId ?? a.user_id ?? null,
    addonId: a.addonId ?? a.addon_id,
    providerId: a.providerId ?? a.provider_id,
    email,
    displayName,
    tenantDomain: extractTenant(email, a.providerId ?? a.provider_id),
    tokenType: a.tokenType ?? a.token_type,
    scopes,
    scopesCount: scopes.length,
    expiresAtEpoch: expiresAt,
    createdAtEpoch: a.createdAtEpoch ?? a.created_at_epoch ?? 0,
    lastUsedAtEpoch: a.lastUsedAtEpoch ?? a.last_used_at_epoch ?? null,
    revoked,
    status,
  };
}

// Domena tenanta wywodzona z emaila (Microsoft/Google). Brak dopasowania -> null.
function extractTenant(email, providerId) {
  if (!email || !email.includes('@')) return null;
  const domain = email.split('@')[1] || '';
  if (!domain) return null;
  return domain;
}

function render(container) {
  const counts = computeCounts(accounts);
  const hasAny = counts.active + counts.expired > 0;

  container.innerHTML = `
    <div class="linked-accounts-header">
      <div class="linked-accounts-header-title">${escapeHtml(I18n.t('addon_oauth.linked_header_title').replace('{n}', counts.all))}</div>
      ${hasAny ? `<tf-button variant="danger" size="sm" id="la-revoke-all" icon="trash">${escapeHtml(I18n.t('addon_oauth.linked_header_revoke_all'))}</tf-button>` : ''}
    </div>

    <div class="linked-accounts-toolbar">
      <tf-searchbox id="la-search" placeholder="${escapeAttr(I18n.t('addon_oauth.linked_search_placeholder'))}" debounce="250" value="${escapeAttr(filterSearch)}"></tf-searchbox>
      <tf-chip clickable ${filterStatus === 'all' ? 'active' : ''} data-filter="all">${escapeHtml(I18n.t('addon_oauth.linked_filter_all'))} (${counts.all})</tf-chip>
      <tf-chip clickable status="ok" ${filterStatus === 'active' ? 'active' : ''} data-filter="active">${escapeHtml(I18n.t('addon_oauth.linked_filter_active'))} (${counts.active})</tf-chip>
      <tf-chip clickable status="warn" ${filterStatus === 'expired' ? 'active' : ''} data-filter="expired">${escapeHtml(I18n.t('addon_oauth.linked_filter_expired'))} (${counts.expired})</tf-chip>
      <tf-chip clickable ${filterStatus === 'revoked' ? 'active' : ''} data-filter="revoked">${escapeHtml(I18n.t('addon_oauth.linked_filter_revoked'))} (${counts.revoked})</tf-chip>
    </div>

    <div class="section-card" style="padding: 0; overflow: auto;">
      <tf-table id="la-table" sortable>
        <tf-column key="user" label="${escapeAttr(I18n.t('addon_oauth.linked_col_user'))}" renderer="html" sortable></tf-column>
        <tf-column key="account" label="${escapeAttr(I18n.t('addon_oauth.linked_col_account'))}" renderer="html" sortable></tf-column>
        <tf-column key="connected" label="${escapeAttr(I18n.t('addon_oauth.linked_col_connected'))}" sortable></tf-column>
        <tf-column key="lastUsed" label="${escapeAttr(I18n.t('addon_oauth.linked_col_last_used'))}" renderer="html" sortable></tf-column>
        <tf-column key="statusChip" label="${escapeAttr(I18n.t('addon_oauth.linked_col_status'))}" renderer="chip"></tf-column>
        <tf-column key="scopesChip" label="${escapeAttr(I18n.t('addon_oauth.linked_col_scopes'))}" renderer="html" align="num"></tf-column>
        <tf-column key="actions" label="${escapeAttr(I18n.t('addon_oauth.linked_col_actions'))}" renderer="html"></tf-column>
      </tf-table>
    </div>
  `;

  // Search
  container.querySelector('#la-search')?.addEventListener('search', (e) => {
    filterSearch = (e.detail?.value || '').toLowerCase();
    renderRows(container);
  });

  // Filter chips
  container.querySelectorAll('tf-chip[data-filter]').forEach((chip) => {
    chip.addEventListener('click', () => {
      filterStatus = chip.getAttribute('data-filter') || 'all';
      // Refresh to update chip active state
      render(container);
    });
  });

  // Revoke-all header action
  container.querySelector('#la-revoke-all')?.addEventListener('click', async () => {
    const ok = await TfWindow.confirm({
      title: I18n.t('addon_oauth.linked_revoke_all_confirm_title'),
      message: I18n.t('addon_oauth.linked_revoke_all_confirm_body'),
      confirmLabel: I18n.t('addon_oauth.linked_header_revoke_all'),
      cancelLabel: I18n.t('common.cancel'),
      danger: true,
    });
    if (!ok) return;
    const targets = accounts.filter((a) => !a.revoked);
    let revokedCount = 0;
    for (const a of targets) {
      try {
        await ApiBinary.action('addonOAuthRevokeRequest', { accountId: a.id });
        a.revoked = true;
        a.status = 'revoked';
        revokedCount += 1;
      } catch (err) {
        // Continue revoking other accounts; errors surface at the end.
      }
    }
    toast(
      I18n.t('addon_oauth.linked_revoke_all_success').replace('{count}', String(revokedCount)),
      'success',
    );
    render(container);
  });

  // Delegated per-row action handler
  const tbl = container.querySelector('#la-table');
  tbl?.addEventListener('click', async (ev) => {
    const path = ev.composedPath();
    const btnHost = path.find((el) => el && el.tagName === 'TF-BUTTON' && el.dataset && el.dataset.role);
    if (!btnHost) return;
    const role = btnHost.dataset.role;
    const id = Number(btnHost.dataset.accountId);
    if (!id) return;
    const acc = accounts.find((a) => a.id === id);
    if (role === 'revoke') {
      const confirmed = await TfWindow.confirm({
        title: I18n.t('addon_oauth.linked_revoke_confirm_title'),
        message: I18n.t('addon_oauth.linked_revoke_confirm_body').replace('{email}', acc?.email || ''),
        confirmLabel: I18n.t('addon_oauth.revoke'),
        cancelLabel: I18n.t('common.cancel'),
        danger: true,
      });
      if (!confirmed) return;
      try {
        await ApiBinary.action('addonOAuthRevokeRequest', { accountId: id });
        if (acc) { acc.revoked = true; acc.status = 'revoked'; }
        toast(
          I18n.t('addon_oauth.linked_revoke_success').replace('{email}', acc?.email || ''),
          'success',
        );
        render(container);
      } catch (err) {
        toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
      }
    } else if (role === 'reauth') {
      try {
        await runOAuthPopup({ accountIdForReauth: id });
        // Reauth refreshes expiry/last_used server-side; fetch only the
        // updated account and patch its row instead of rebuilding the table.
        await reloadAccount(id);
        toast(I18n.t('common.saved'), 'success');
        renderRows(container);
      } catch (err) {
        toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
      }
    }
  });

  renderRows(container);
}

function computeCounts(list) {
  const counts = { all: list.length, active: 0, expired: 0, revoked: 0 };
  list.forEach((a) => {
    counts[a.status] = (counts[a.status] || 0) + 1;
  });
  return counts;
}

function renderRows(container) {
  const tbl = container.querySelector('#la-table');
  if (!tbl) return;
  const filtered = accounts.filter((a) => {
    if (filterStatus !== 'all' && a.status !== filterStatus) return false;
    if (filterSearch) {
      const hay = `${a.displayName} ${a.email} ${a.userId ?? ''}`.toLowerCase();
      if (!hay.includes(filterSearch)) return false;
    }
    return true;
  });
  if (filtered.length === 0) {
    tbl.rows = [];
    // Show an empty-state hint inside the table wrapper
    const host = tbl.parentElement;
    if (host && !host.querySelector('.linked-accounts-empty')) {
      const empty = document.createElement('div');
      empty.className = 'linked-accounts-empty';
      empty.textContent = I18n.t('addon_oauth.linked_empty');
      host.appendChild(empty);
    }
    return;
  }
  const host = tbl.parentElement;
  host?.querySelector('.linked-accounts-empty')?.remove();

  const rows = filtered.map((a) => {
    const chipStatus = a.status === 'active' ? 'ok' : (a.status === 'expired' ? 'warn' : 'err');
    const statusLabel = I18n.t('addon_oauth.linked_status_' + a.status);
    const initials = makeInitials(a.displayName || a.email);
    const userCell = `
      <div class="account-cell">
        <div class="account-avatar">${escapeHtml(initials)}</div>
        <div>
          <div style="font-weight:600;">${escapeHtml(a.displayName || ('user #' + (a.userId ?? '—')))}</div>
          <div style="color:var(--text-3);font-size:11px;">${a.userId != null ? 'user #' + a.userId : '—'}</div>
        </div>
      </div>
    `;
    const accountCell = `
      <div class="account-email-mono">${escapeHtml(a.email)}</div>
      ${a.tenantDomain ? `<div class="account-tenant">${escapeHtml(a.tenantDomain)}</div>` : ''}
    `;
    const lastUsed = a.lastUsedAtEpoch
      ? `<div>${escapeHtml(fmtEpoch(a.lastUsedAtEpoch))}</div><div class="account-tenant">${escapeHtml(formatRelative(a.lastUsedAtEpoch))}</div>`
      : '<span style="color:var(--text-3);">—</span>';
    const scopesChip = `<span class="tf-chip info">${a.scopesCount}</span>`;
    let actions;
    if (a.status === 'revoked') {
      actions = `<span style="color:var(--text-3);font-size:11px;">${escapeHtml(I18n.t('addon_oauth.linked_status_revoked'))}</span>`;
    } else if (a.status === 'expired') {
      actions = `
        <tf-button size="sm" variant="primary" data-role="reauth" data-account-id="${a.id}">${escapeHtml(I18n.t('addon_oauth.linked_reauth_button'))}</tf-button>
        <tf-button size="sm" variant="ghost" icon="unlink" data-role="revoke" data-account-id="${a.id}" title="${escapeAttr(I18n.t('addon_oauth.revoke'))}"></tf-button>
      `;
    } else {
      actions = `<tf-button size="sm" variant="ghost" data-role="revoke" data-account-id="${a.id}">${escapeHtml(I18n.t('addon_oauth.revoke'))}</tf-button>`;
    }
    return {
      user: userCell,
      account: accountCell,
      connected: fmtEpoch(a.createdAtEpoch),
      lastUsed,
      statusChip: { status: chipStatus, label: statusLabel, dot: true },
      scopesChip,
      actions,
    };
  });
  tbl.rows = rows;
}

function makeInitials(name) {
  if (!name) return '?';
  const parts = String(name).split(/[\s@._-]+/).filter(Boolean);
  if (parts.length === 0) return String(name).charAt(0).toUpperCase();
  const first = parts[0].charAt(0);
  const second = parts.length > 1 ? parts[1].charAt(0) : '';
  return (first + second).toUpperCase();
}

function fmtEpoch(e) {
  if (!e) return '—';
  try {
    const d = new Date(Number(e) * 1000);
    const pad = (n) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  } catch {
    return '—';
  }
}

function formatRelative(epochSec) {
  if (!epochSec) return '';
  const diff = Math.floor(Date.now() / 1000) - Number(epochSec);
  if (diff < 60) return I18n.t('addon_oauth.linked_relative_just_now');
  if (diff < 3600) {
    return I18n.t('addon_oauth.linked_relative_min_ago').replace('{n}', String(Math.floor(diff / 60)));
  }
  if (diff < 86400) {
    return I18n.t('addon_oauth.linked_relative_hours_ago').replace('{n}', String(Math.floor(diff / 3600)));
  }
  return I18n.t('addon_oauth.linked_relative_days_ago').replace('{n}', String(Math.floor(diff / 86400)));
}
