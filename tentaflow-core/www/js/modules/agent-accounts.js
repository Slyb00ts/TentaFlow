// =============================================================================
// File: modules/agent-accounts.js — agent provider accounts: the ONE data and
//       label layer behind the admin list (A01), the account window (A03/A04),
//       the node matrix (N01), "Moje konta" (U01) and the agent editor (G01).
//
//       Every request goes through the binary protocol family
//       `ProviderAccountBody`. Nothing here ever handles credential material
//       except the one field of `providerAccountCredentialSetRequest`, which is
//       write-only: no response carries a key back, so no screen can show one.
//
//       Login, session revoke and CLI installation are NOT here, because this
//       node refuses them with NotAvailable — the screens leave the affordance
//       out instead of rendering a button that cannot work.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';

/** i18n shorthand for the `agent_accounts.*` namespace. */
export function T(key, vars) {
  return I18n.t(`agent_accounts.${key}`, vars);
}

// =============================================================================
// Requests
// =============================================================================

export const AgentAccounts = {
  /** A01 — every account for an administrator, own + granted for anybody else. */
  list({ engineId = null, scope = null, query = null } = {}) {
    return ApiBinary.one('providerAccountListRequest', { engineId, scope, query });
  },
  /** A03/A04 — one account with its grants, sessions, nodes and agents. */
  get(accountId) {
    return ApiBinary.one('providerAccountGetRequest', { accountId });
  },
  create({ engineId, displayName, scope, credentialKind }) {
    return ApiBinary.action('providerAccountCreateRequest', {
      engineId, displayName, scope, credentialKind,
    });
  },
  /** Omitted fields stay as they are — `null` is "do not touch". */
  update({ accountId, displayName = null, status = null }) {
    return ApiBinary.action('providerAccountUpdateRequest', { accountId, displayName, status });
  },
  remove(accountId) {
    return ApiBinary.action('providerAccountDeleteRequest', { accountId });
  },
  setCredential(accountId, material) {
    return ApiBinary.action('providerAccountCredentialSetRequest', { accountId, material });
  },
  clearCredential(accountId) {
    return ApiBinary.action('providerAccountCredentialClearRequest', { accountId });
  },
  /** Full replace: whatever is not in `grants` is revoked. */
  setGrants(accountId, grants) {
    return ApiBinary.action('providerAccountGrantsSetRequest', { accountId, grants });
  },
  sessions(accountId) {
    return ApiBinary.one('providerAccountSessionListRequest', { accountId });
  },
  /** U01 — the caller's own accounts plus the shared ones granted to them. */
  mine({ engineId = null } = {}) {
    return ApiBinary.one('providerAccountMyListRequest', { engineId });
  },
  runtimeNodes() {
    return ApiBinary.one('providerAccountRuntimeListRequest', {});
  },
  setReceivesAccounts(nodeId, enabled) {
    return ApiBinary.action('providerAccountRuntimeSetReceivesAccountsRequest', { nodeId, enabled });
  },
};

// =============================================================================
// Labels and chips
// =============================================================================

// The engine tile: two letters on one of the shared `tf-table__tile--tN`
// gradients. Those classes live in controls.css, the only sheet adopted into a
// tf-table shadow root, so the same markup works in a cell and in a card.
const ENGINE_TILES = {
  'claude-code': { code: 'CC', tile: 'tf-table__tile--t1' },
  codex: { code: 'CX', tile: 'tf-table__tile--t2' },
  'grok-build': { code: 'GK', tile: 'tf-table__tile--t3' },
  'muse-code': { code: 'MS', tile: 'tf-table__tile--t4' },
};

/** Two-letter fallback for an engine this build does not know by name. */
function fallbackCode(engineId) {
  const letters = String(engineId || '?').replace(/[^a-z]/gi, '').toUpperCase();
  return letters.slice(0, 2) || '??';
}

export function engineTile(engineId) {
  const known = ENGINE_TILES[engineId];
  const code = known?.code ?? fallbackCode(engineId);
  const tile = known?.tile ?? 'tf-table__tile--t5';
  return `<span class="tf-table__tile ${tile}">${escapeHtml(code)}</span>`;
}

/**
 * Human name of an engine. `engines` is the catalog from
 * `AccountListResponse`; an engine missing from it keeps its id rather than
 * being renamed by a guess.
 */
export function engineName(engineId, engines = []) {
  const entry = engines.find((e) => (e.engine_id ?? e.engineId) === engineId);
  return entry?.display_name ?? entry?.displayName ?? String(engineId || '');
}

/**
 * The status chip. `status` and `credential_kind` answer different halves of
 * one question — an active subscription is "signed in", an active API-key
 * account is "verified" — and a pending one says which of the two it is still
 * waiting for, so the operator knows what to do next.
 */
export function statusChipHtml(account) {
  const status = String(account.status || '');
  const apiKey = (account.credential_kind ?? account.credentialKind) === 'api_key';
  const map = {
    active: { tone: 'ok', key: apiKey ? 'status_active_api_key' : 'status_active_login' },
    needs_login: { tone: 'warn', key: apiKey ? 'status_needs_key' : 'status_needs_login' },
    pending: { tone: 'info', key: apiKey ? 'status_pending_api_key' : 'status_pending_login' },
    disabled: { tone: 'neutral', key: 'status_disabled' },
  };
  const entry = map[status] ?? { tone: 'neutral', key: 'status_unknown' };
  const label = T(entry.key);
  return `<tf-chip size="sm" status="${entry.tone}" dot label="${escapeAttr(label)}"></tf-chip>`;
}

export function scopeChipHtml(scope) {
  const global = scope === 'global';
  const label = T(global ? 'scope_global' : 'scope_user');
  return `<tf-chip size="sm" status="${global ? 'accent' : 'neutral'}" label="${escapeAttr(label)}"></tf-chip>`;
}

export function credentialKindLabel(kind) {
  return T(kind === 'api_key' ? 'credential_api_key' : 'credential_subscription');
}

/** Short form of a uuid for a sub-line; never a title on its own. */
export function shortId(id) {
  return String(id || '').slice(0, 8);
}

/**
 * The sub-line of an account's name cell: the provider identity and the plan
 * when the account has been signed in, the owner for a personal account, and
 * nothing invented when neither is known.
 */
export function accountSubtitle(account) {
  const parts = [];
  const subject = account.provider_subject ?? account.providerSubject;
  const plan = account.plan_label ?? account.planLabel;
  if (subject) parts.push(subject);
  if (plan) parts.push(plan);
  if (!parts.length) {
    const owner = account.owner_display_name ?? account.ownerDisplayName;
    if (owner) parts.push(owner);
    else if (account.scope === 'user') parts.push(T('own_account'));
  }
  return parts.join(' · ');
}

/** `absent | materializing | ready | error` → what the node column says. */
export function nodeCredentialLabel(node, accountRevision) {
  const state = String(node.runtime_state ?? node.runtimeState ?? '');
  const applied = Number(node.applied_revision ?? node.appliedRevision ?? 0);
  if (state === 'error') return { tone: 'err', label: T('node_state_error') };
  if (state === 'materializing') return { tone: 'info', label: T('node_state_materializing') };
  if (state === 'ready') {
    return applied >= Number(accountRevision || 0)
      ? { tone: 'ok', label: T('node_state_current') }
      : { tone: 'warn', label: T('node_state_stale') };
  }
  return { tone: 'neutral', label: T('node_state_on_first_use') };
}

/**
 * "14 min", "1 h 02 min" — the A03 sessions table shows for how long a session
 * has been open, not when it started. An unparseable timestamp stays unknown
 * instead of printing an epoch.
 */
export function sinceLabel(isoTimestamp) {
  const started = Date.parse(String(isoTimestamp || ''));
  if (!Number.isFinite(started)) return '—';
  const minutes = Math.max(0, Math.round((Date.now() - started) / 60000));
  if (minutes < 60) return T('since_minutes', { n: minutes });
  const hours = Math.floor(minutes / 60);
  return T('since_hours', { h: hours, m: String(minutes % 60).padStart(2, '0') });
}

/** Local date+time of an ISO timestamp, or an em dash when there is none. */
export function whenLabel(isoTimestamp) {
  const at = Date.parse(String(isoTimestamp || ''));
  if (!Number.isFinite(at)) return '—';
  return new Date(at).toLocaleString(undefined, {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit',
  });
}

/** The engine ids an account may be created for, given what the engine supports. */
export function credentialKindsFor(engine) {
  const kinds = [];
  if (engine?.supports_login ?? engine?.supportsLogin) kinds.push('provider_login');
  if (engine?.supports_api_key ?? engine?.supportsApiKey) kinds.push('api_key');
  return kinds;
}

export function engineEntry(engines, engineId) {
  return engines.find((e) => (e.engine_id ?? e.engineId) === engineId) ?? null;
}
