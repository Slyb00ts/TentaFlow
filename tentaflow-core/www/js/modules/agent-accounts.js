// =============================================================================
// File: modules/agent-accounts.js — agent provider accounts: the ONE data and
//       label layer behind the admin list (A01), the account window (A03/A04),
//       the node matrix (N01), "Moje konta" (U01) and the agent editor (G01).
//
//       Every request goes through the binary protocol family
//       `ProviderAccountBody`. Nothing here ever handles credential material
//       except the one field of `providerAccountCredentialSetRequest`, which is
//       write-only: no response carries a key back, so no screen can show one.
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

/**
 * Deadlines for the sign-in, in milliseconds.
 *
 * `provider_accounts/login.rs` gives the CLI 45 s to print its verification
 * address (`URL_TIMEOUT`) and spends more before that materializing the
 * credential and starting the bridge; the shim's default call deadline is 30 s
 * (`protocol/api-binary-shim.js`), which is why a start on a healthy node used
 * to end in "timed out after 30000ms" while the terminal was still opening.
 */
export const LOGIN_START_TIMEOUT_MS = 90_000;
/** Typing a code and closing a terminal both go through the bridge on the node. */
export const LOGIN_STEP_TIMEOUT_MS = 60_000;

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
  /** Ends ONE live session of an account; the credential is untouched. */
  revokeSession(accountId, sessionId) {
    return ApiBinary.action('providerAccountSessionRevokeRequest', { accountId, sessionId });
  },
  /**
   * A02 — the provider sign-in. Core runs the vendor CLI on `nodeId` (its home
   * node, or this one, when the caller names none) and answers with the address
   * the person has to open; every later step addresses the flow by its id.
   *
   * The deadline is the caller's, not the shim's default: `login.rs` waits
   * `URL_TIMEOUT` = 45 s for the CLI to print an address, on top of
   * materializing the credential and starting the bridge. With the shim's 30 s
   * the browser gave up first and reported a timeout for a terminal the node
   * was still opening.
   */
  loginStart({ accountId, nodeId = null }) {
    return ApiBinary.action(
      'providerAccountLoginStartRequest',
      { accountId, nodeId },
      { timeoutMs: LOGIN_START_TIMEOUT_MS },
    );
  },
  /** Typing into the terminal goes through the bridge, so it gets the long deadline too. */
  loginInput(loginId, value) {
    return ApiBinary.action(
      'providerAccountLoginInputRequest',
      { loginId, value },
      { timeoutMs: LOGIN_STEP_TIMEOUT_MS },
    );
  },
  /** A poll is retried by the caller, so it keeps the shim's ordinary deadline. */
  loginStatus(loginId) {
    return ApiBinary.one('providerAccountLoginStatusRequest', { loginId });
  },
  /** Cancelling closes a PTY on the node; it must not give up before the node does. */
  loginCancel(loginId) {
    return ApiBinary.action(
      'providerAccountLoginCancelRequest',
      { loginId },
      { timeoutMs: LOGIN_STEP_TIMEOUT_MS },
    );
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
  /** N01 — installs the vendor CLI and its bridge for one engine on one node. */
  installRuntime(nodeId, engineId) {
    return ApiBinary.action('providerAccountRuntimeInstallRequest', { nodeId, engineId });
  },
  uninstallRuntime(nodeId, engineId) {
    return ApiBinary.action('providerAccountRuntimeUninstallRequest', { nodeId, engineId });
  },
};

// =============================================================================
// Refusals
// =============================================================================

// A refusal Core spells out in English, and the key that says the same thing in
// the operator's language.
//
// `ProtocolError` carries `code` + `message` + `trace_id` and NO key field
// (`tentaflow-protocol/src/message_body.rs`), so for these handlers the English
// sentence IS the contract. Every marker below is a literal from the node:
// `services/agent_runtime.rs` (NOT_RECEIVING_ACCOUNTS, "is not installed on
// this node"), `provider_accounts/login.rs` (the three ways a start can end
// without an address) and `dispatch/provider_account.rs` (the two refusals
// `require_login_authority` raises). Matched by substring, because an anyhow
// chain prefixes each with the operation that hit it, and the `code` narrows
// the match so an unrelated sentence cannot borrow a translation.
//
// The last entry is the BROWSER's own deadline (`api-binary-shim.js`), which
// arrives with no protocol prefix at all.
const REFUSALS = [
  { code: 'PolicyDenied', marker: 'is not configured to receive agent accounts', key: 'login.node_not_receiving' },
  { code: 'NotAvailable', marker: 'is not installed on this node', key: 'login.engine_missing' },
  { code: 'NotAvailable', marker: 'failed before it showed an address', key: 'login.no_address_failed' },
  { code: 'NotAvailable', marker: 'ended before it showed an address', key: 'login.no_address_closed' },
  { code: 'NotAvailable', marker: 'did not show a sign-in address in time', key: 'login.no_address_timeout' },
  { code: 'NotAvailable', marker: 'the bridge started no sign-in', key: 'login.no_terminal' },
  { code: 'BadRequest', marker: 'nothing to sign in to', key: 'login.not_a_login_account' },
  { code: 'PolicyDenied', marker: 'this account is disabled', key: 'login.account_disabled' },
  { code: 'Conflict', marker: 'sign-in for this account is already running', key: 'login.already_running' },
  { code: 'PolicyDenied', marker: "personal account is the owner's alone", key: 'login.owner_only' },
  { code: '', marker: 'timed out after', key: 'error_timeout' },
];

/**
 * A server refusal as a screen may render it.
 *
 * `message` is what the operator reads: the translation when the node sent one
 * of the refusals above, the server's own sentence otherwise. `detail` is that
 * sentence whenever it was replaced, so a screen can keep it next to the
 * translation — a mapped message must not be the only thing left of what the
 * node said. `code` is the wire enum the caller reacts to (`NotFound` while
 * polling).
 *
 * The `protocol error <Code>: ` prefix is added by `binary-ws-client.js` when
 * it rejects a call; it names a wire enum variant and belongs in a log, not in
 * a sentence an operator reads.
 */
export function describeError(error) {
  const raw = String(error?.message ?? error ?? '').trim();
  const parsed = /^protocol error ([A-Za-z]+):\s*([\s\S]*)$/.exec(raw);
  const code = parsed ? parsed[1] : '';
  const message = (parsed ? parsed[2] : raw).trim();
  const known = REFUSALS.find(
    (entry) => (entry.code === '' || entry.code === code) && message.includes(entry.marker),
  );
  if (known) return { code, message: T(known.key), detail: message };
  return { code, message: message || T('error_unknown'), detail: '' };
}

/** The same refusal as one string, for a toast or an error line. */
export function errorText(error) {
  return describeError(error).message;
}

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
 *
 * A row that carries no `credential_kind` (the caller's own accounts, which the
 * wire returns without it) falls back to wording that holds for both kinds: a
 * key account must never be labelled "signed in".
 */
export function statusChipHtml(account) {
  const status = String(account.status || '');
  const kind = account.credential_kind ?? account.credentialKind ?? null;
  const apiKey = kind === 'api_key';
  const forKind = (keyApiKey, keyLogin, keyAny) => (kind === null ? keyAny : (apiKey ? keyApiKey : keyLogin));
  const map = {
    active: { tone: 'ok', key: forKind('status_active_api_key', 'status_active_login', 'status_active_any') },
    needs_login: { tone: 'warn', key: forKind('status_needs_key', 'status_needs_login', 'status_needs_any') },
    pending: { tone: 'info', key: forKind('status_pending_api_key', 'status_pending_login', 'status_pending_any') },
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

/**
 * Date+time of an ISO timestamp in the APP's language (the one the operator
 * picked), not the browser's — the rest of the dashboard formats the same way.
 * An em dash when there is no usable timestamp.
 */
export function whenLabel(isoTimestamp) {
  const at = Date.parse(String(isoTimestamp || ''));
  if (!Number.isFinite(at)) return '—';
  return new Date(at).toLocaleString(I18n.getLanguage() || undefined, {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit',
  });
}

/**
 * A01/U01 "Używane na": the nodes that hold this account's credential, named.
 *
 * The wire says this is what the ANSWERING node measured, never a fleet view
 * (`provider_account_node_state` stays out of the ledger), so an empty list is
 * "nothing reported here" — an em dash, and the tooltip next to the column says
 * the rest.
 */
export function usedOnLabel(account) {
  const nodes = account.used_on ?? account.usedOn ?? [];
  const names = nodes.map((node) => node.node_name ?? node.nodeName ?? '').filter(Boolean);
  return names.length ? names.join(', ') : T('value_none');
}

/** `linux` / `macos` / `windows` as the operator's language spells it. */
export function osLabel(os) {
  const id = String(os || '');
  if (!id) return T('runtime_os_unknown');
  const known = { linux: 'runtime_os_linux', macos: 'runtime_os_macos', windows: 'runtime_os_windows' };
  return known[id] ? T(known[id]) : id;
}

/** A04 "Nadał": who granted access and when, or nothing when neither is known. */
export function grantedByLabel(grant) {
  const who = grant.granted_by_name ?? grant.grantedByName ?? '';
  const when = grant.granted_at ?? grant.grantedAt ?? '';
  const parts = [who, when ? whenLabel(when) : ''].filter(Boolean);
  return parts.length ? parts.join(' · ') : T('value_none');
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
