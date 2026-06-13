// =============================================================================
// File: modules/services/access.js — shared access-control rendering for the
//   Services screen. Drives M16b (alias visibility + consumers) and M8b (model
//   visibility + consumers). Both screens share the same primitives:
//     - a 3-state (alias) or 2-state (model) visibility tf-segmented
//     - a lazily-loaded consumer table (granted/pending/revoked) with
//       grant/revoke tf-buttons
//   State is keyed by alias id / model id so multiple expanded panels coexist.
//   Wire helpers used here:
//     alias:  aliasVisibilitySetRequest, aliasConsumerListRequest,
//             aliasConsumerGrantRequest, aliasConsumerRevokeRequest
//     model:  modelVisibilitySetRequest, modelConsumerListRequest,
//             modelConsumerGrantRequest, modelConsumerRevokeRequest
//   The owning module (services.js) calls renderConsumerPanel() inside the
//   expanded row, then bindAccessEvents() after the DOM patch, and supplies a
//   `rerender` callback so optimistic transitions can re-paint in place.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

// Per-scope cache of consumer lists + per-row UI state. scope is 'alias' or
// 'model'; id is the alias id (number) or model id (string). Keeping caches
// separated means an alias id 7 and model id "7" never collide.
//   consumerCache: Map<cacheKey, { loading, error, consumers[] }>
//   visibilityCache: Map<cacheKey, visibility>   (optimistic mirror)
const consumerCache = new Map();
const visibilityCache = new Map();

function cacheKey(scope, id) {
  return `${scope}:${String(id)}`;
}

// Wire request kinds per scope. Centralised so the two screens stay in lockstep
// and a typo cannot diverge alias vs model behaviour.
const WIRE = {
  alias: {
    visibilitySet: 'aliasVisibilitySetRequest',
    consumerList: 'aliasConsumerListRequest',
    grant: 'aliasConsumerGrantRequest',
    revoke: 'aliasConsumerRevokeRequest',
    idKey: 'aliasId',
  },
  model: {
    visibilitySet: 'modelVisibilitySetRequest',
    consumerList: 'modelConsumerListRequest',
    grant: 'modelConsumerGrantRequest',
    revoke: 'modelConsumerRevokeRequest',
    idKey: 'modelId',
  },
};

// Visibility option sets per scope. Aliases have the full 3-state model;
// models are restricted/public only (no per-model "private" — a model with no
// consumers is simply restricted with an empty whitelist).
const VISIBILITY_OPTIONS = {
  alias: ['private', 'restricted', 'public'],
  model: ['restricted', 'public'],
};

const VISIBILITY_ICON = {
  private: 'lock',
  restricted: 'shield',
  public: 'unlock',
};

export function visibilityIcon(v) {
  return VISIBILITY_ICON[String(v || '').toLowerCase()] || 'shield';
}

// Derive grant state from the consumer timeline. The wire never sends a literal
// "pending"/"granted" enum — it sends the grant lifecycle timestamps. A row
// with revokedAt is revoked; with grantedAt (and no revoke) it is granted;
// with neither it is a self-declared, not-yet-approved request (pending).
// An explicit grantStatus, when present, wins so a backend that later sends a
// derived enum stays authoritative.
function consumerState(c) {
  const explicit = String(c.grantStatus || c.grant_status || '').toLowerCase();
  if (explicit === 'granted' || explicit === 'pending'
      || explicit === 'revoked' || explicit === 'denied') {
    return explicit;
  }
  const revoked = c.revokedAt || c.revoked_at;
  const granted = c.grantedAt || c.granted_at;
  if (revoked) return 'revoked';
  if (granted) return 'granted';
  return 'pending';
}

// tf-status-pill tone per grant state (ok=success, warn=pending, err=danger).
function statePill(state) {
  const tone = state === 'granted' ? 'ok'
    : state === 'pending' ? 'warn'
      : 'err';
  return `<tf-status-pill status="${tone}" label="${escapeAttr(I18n.t(`services.access_state_${state}`))}"></tf-status-pill>`;
}

function consumerAddonId(c) {
  return c.addonId || c.addon_id || '';
}

function consumerGrantedBy(c) {
  const by = c.grantedByUserId || c.granted_by_user_id || '';
  return by || I18n.t('services.access_consumer_self');
}

function formatWhen(c) {
  // Prefer the most recent lifecycle timestamp for the "when" column.
  const raw = c.revokedAt ?? c.revoked_at ?? c.grantedAt ?? c.granted_at;
  if (raw === null || raw === undefined || raw === '') return '—';
  // The wire sends Unix SECONDS as a number; Date() expects ms, so numeric
  // values must be scaled. Optimistic updates write ISO strings, left as-is.
  const d = typeof raw === 'number' ? new Date(raw * 1000) : new Date(raw);
  if (Number.isNaN(d.getTime())) return String(raw);
  return d.toLocaleString();
}

// ---- Visibility segmented -------------------------------------------------

// Returns the effective visibility for a scope/id: the optimistic cache wins
// (set by a successful aliasVisibilitySetRequest), otherwise the row's own
// value when it is a real visibility. When the row carries no visibility (the
// list endpoint does not yet expose it) the result is 'unknown' — never a false
// 'restricted'. A batch alias-visibility list endpoint is a backend follow-up
// so collapsed rows can show the real state.
export function effectiveVisibility(scope, id, fromRow) {
  const key = cacheKey(scope, id);
  if (visibilityCache.has(key)) return visibilityCache.get(key);
  const v = String(fromRow || '').toLowerCase();
  if (VISIBILITY_OPTIONS[scope].includes(v)) return v;
  return 'unknown';
}

function renderVisibilitySegmented(scope, id, current) {
  const opts = VISIBILITY_OPTIONS[scope];
  const isUnknown = !VISIBILITY_OPTIONS[scope].includes(current);
  const options = opts.map((v) => {
    const variant = v === 'public' ? 'ok' : v === 'private' ? 'err' : 'warn';
    return `<option value="${v}" variant="${variant}">${escapeHtml(I18n.t(`services.alias_visibility_${v}`))}</option>`;
  }).join('');
  // Unknown real visibility: leave the segmented unset + disabled with a hint
  // instead of pre-selecting a value the admin never chose.
  const hint = isUnknown
    ? I18n.t('services.access_visibility_hint_unknown')
    : I18n.t(`services.access_visibility_hint_${current}`);
  return `
    <div class="svc-access-vis">
      <label class="svc-access-vis-label">${escapeHtml(I18n.t('services.access_visibility_label'))}</label>
      <tf-segmented ${isUnknown ? 'disabled' : `value="${escapeAttr(current)}"`} size="sm"
                    data-access-vis="${escapeAttr(scope)}"
                    data-access-id="${escapeAttr(String(id))}">
        ${options}
      </tf-segmented>
      <div class="svc-access-vis-hint">${escapeHtml(hint)}</div>
    </div>
  `;
}

// ---- Consumer panel -------------------------------------------------------

// Renders the full expandable access panel (visibility + consumer table) for a
// single alias/model. `current` is the effective visibility, `name` the display
// label used in the header. The consumer table is populated lazily — on first
// render the cache is empty so a loading state shows; bindAccessEvents triggers
// the fetch.
export function renderConsumerPanel(scope, id, name, currentVisibility) {
  const key = cacheKey(scope, id);
  const cache = consumerCache.get(key);
  const visSegment = renderVisibilitySegmented(scope, id, currentVisibility);

  let tableHtml;
  if (!cache || cache.loading) {
    tableHtml = `<div class="svc-access-loading">${sprite('rotate')} ${escapeHtml(I18n.t('common.loading'))}</div>`;
  } else if (cache.error) {
    tableHtml = `<div class="svc-access-error">${escapeHtml(cache.error)}</div>`;
  } else if (!cache.consumers || cache.consumers.length === 0) {
    tableHtml = `<div class="svc-access-empty">${escapeHtml(I18n.t('services.access_consumers_empty'))}</div>`;
  } else {
    tableHtml = renderConsumerTable(scope, id, cache.consumers);
  }

  const count = cache && Array.isArray(cache.consumers) ? cache.consumers.length : 0;

  return `
    <div class="svc-access-panel" data-access-panel="${escapeAttr(scope)}" data-access-panel-id="${escapeAttr(String(id))}">
      <div class="svc-access-head">
        ${sprite('eye')}
        <span class="svc-access-title">${escapeHtml(I18n.t('services.access_panel_title'))}</span>
        <code class="svc-access-name">${escapeHtml(name)}</code>
      </div>
      ${visSegment}
      <div class="svc-access-consumers-head">
        <div class="svc-access-consumers-title">
          ${sprite('users')} ${escapeHtml(I18n.t('services.access_consumers_title'))}
          <span class="svc-access-count">${count}</span>
        </div>
      </div>
      <div class="svc-access-consumers-body" data-access-table="${escapeAttr(key)}">
        ${tableHtml}
      </div>
    </div>
  `;
}

function renderConsumerTable(scope, id, consumers) {
  const rows = consumers.map((c) => {
    const addonId = consumerAddonId(c);
    const state = consumerState(c);
    const actions = renderConsumerActions(scope, id, addonId, state);
    return `
      <tr data-key="cons-${escapeAttr(addonId)}" class="${state === 'revoked' ? 'svc-access-row-revoked' : ''}">
        <td data-label="${escapeAttr(I18n.t('services.access_col_addon'))}">
          <span class="svc-access-addon">${sprite('puzzle')}${escapeHtml(addonId)}</span>
        </td>
        <td data-label="${escapeAttr(I18n.t('services.access_col_by'))}" class="svc-access-by">${escapeHtml(consumerGrantedBy(c))}</td>
        <td data-label="${escapeAttr(I18n.t('services.access_col_when'))}" class="svc-access-when">${escapeHtml(formatWhen(c))}</td>
        <td data-label="${escapeAttr(I18n.t('services.access_col_status'))}">${statePill(state)}</td>
        <td data-label="${escapeAttr(I18n.t('services.access_col_actions'))}" class="svc-access-actions">${actions}</td>
      </tr>
    `;
  }).join('');

  return `
    <table class="data-table svc-access-table">
      <thead>
        <tr>
          <th>${escapeHtml(I18n.t('services.access_col_addon'))}</th>
          <th>${escapeHtml(I18n.t('services.access_col_by'))}</th>
          <th>${escapeHtml(I18n.t('services.access_col_when'))}</th>
          <th>${escapeHtml(I18n.t('services.access_col_status'))}</th>
          <th style="text-align:right;">${escapeHtml(I18n.t('services.access_col_actions'))}</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>
  `;
}

function renderConsumerActions(scope, id, addonId, state) {
  const grantBtn = `<tf-button variant="primary" size="sm" icon="check"
      data-access-grant="${escapeAttr(addonId)}"
      data-access-scope="${escapeAttr(scope)}"
      data-access-id="${escapeAttr(String(id))}">${escapeHtml(I18n.t('services.access_grant'))}</tf-button>`;
  const revokeBtn = `<tf-button variant="danger" size="sm" icon="x"
      data-access-revoke="${escapeAttr(addonId)}"
      data-access-scope="${escapeAttr(scope)}"
      data-access-id="${escapeAttr(String(id))}">${escapeHtml(I18n.t('services.access_revoke'))}</tf-button>`;
  if (state === 'pending') {
    // Pending self-declared request → admin can approve (grant) or reject (revoke).
    return `<span class="svc-access-btn-pair">${grantBtn}${revokeBtn}</span>`;
  }
  if (state === 'granted') return revokeBtn;
  // revoked / denied → can re-grant.
  return grantBtn;
}

// ---- Lazy loading ---------------------------------------------------------

// Fetches the consumer list for a scope/id and repaints via `rerender`. Marks
// the cache as loading first so the spinner shows on the initial open. When the
// consumer-list response carries the owning scope's real `visibility`, it is
// mirrored into the visibility cache so the previously-'unknown' segmented and
// collapsed chip resolve to the true state.
export async function loadConsumers(scope, id, rerender, { onVisibilityChanged } = {}) {
  const key = cacheKey(scope, id);
  consumerCache.set(key, { loading: true, error: null, consumers: [] });
  if (typeof rerender === 'function') rerender();
  const wire = WIRE[scope];
  try {
    const resp = await ApiBinary.one(wire.consumerList, { [wire.idKey]: scopeId(scope, id) });
    const consumers = Array.isArray(resp?.consumers) ? resp.consumers : [];
    consumerCache.set(key, { loading: false, error: null, consumers });
    const vis = String(resp?.visibility ?? '').toLowerCase();
    if (VISIBILITY_OPTIONS[scope].includes(vis) && !visibilityCache.has(key)) {
      visibilityCache.set(key, vis);
      if (typeof onVisibilityChanged === 'function') onVisibilityChanged(scope, id, vis);
    }
  } catch (err) {
    consumerCache.set(key, { loading: false, error: err.message, consumers: [] });
  }
  if (typeof rerender === 'function') rerender();
}

// Alias ids are numbers on the wire; model ids are strings. Coerce id back to
// the right shape for the request payload.
function scopeId(scope, id) {
  if (scope === 'alias') {
    const n = parseInt(id, 10);
    return Number.isNaN(n) ? id : n;
  }
  return String(id);
}

export function hasConsumerCache(scope, id) {
  return consumerCache.has(cacheKey(scope, id));
}

export function clearAccessCache() {
  consumerCache.clear();
  visibilityCache.clear();
}

// ---- Event binding --------------------------------------------------------

// Binds visibility + grant/revoke handlers inside a freshly-patched root.
// `rerender` repaints the owning tab so optimistic state lands without flicker.
// `onVisibilityChanged(scope, id, visibility)` lets services.js mirror the new
// visibility onto its own row data so the collapsed chip updates too.
export function bindAccessEvents(root, { rerender, onVisibilityChanged } = {}) {
  root.querySelectorAll('[data-access-vis]').forEach((seg) => {
    seg.addEventListener('change', (e) => {
      const scope = seg.dataset.accessVis;
      const id = seg.dataset.accessId;
      const next = String(e.detail?.value ?? seg.value ?? '').toLowerCase();
      if (!scope || !id || !VISIBILITY_OPTIONS[scope]?.includes(next)) return;
      setVisibility(scope, id, next, { rerender, onVisibilityChanged });
    });
  });

  root.querySelectorAll('[data-access-grant]').forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      mutateConsumer('grant', b.dataset.accessScope, b.dataset.accessId, b.dataset.accessGrant, rerender);
    };
  });
  root.querySelectorAll('[data-access-revoke]').forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      mutateConsumer('revoke', b.dataset.accessScope, b.dataset.accessId, b.dataset.accessRevoke, rerender);
    };
  });
}

// Visibility mutation. Optimistically mirrors the chosen value, then applies the
// returned `transitions` (per-consumer before/after grant flips that public/
// restricted toggling causes) onto the cached consumer rows so the table reflows
// without a refetch. On error the optimistic value is reverted and a toast fires.
async function setVisibility(scope, id, visibility, { rerender, onVisibilityChanged } = {}) {
  const key = cacheKey(scope, id);
  const prev = visibilityCache.get(key);
  visibilityCache.set(key, visibility);
  if (typeof onVisibilityChanged === 'function') onVisibilityChanged(scope, id, visibility);
  if (typeof rerender === 'function') rerender();
  const wire = WIRE[scope];
  try {
    const resp = await ApiBinary.action(wire.visibilitySet, {
      [wire.idKey]: scopeId(scope, id),
      visibility,
    });
    applyTransitions(scope, id, resp?.transitions);
    if (typeof rerender === 'function') rerender();
  } catch (err) {
    // Revert optimistic visibility.
    if (prev === undefined) visibilityCache.delete(key);
    else visibilityCache.set(key, prev);
    if (typeof onVisibilityChanged === 'function') {
      onVisibilityChanged(scope, id, prev || '');
    }
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    if (typeof rerender === 'function') rerender();
  }
}

// Applies visibility transitions onto the cached consumer rows. Each transition
// carries { addonId, before, after } where after is the new grant lifecycle
// hint. We translate after→timeline fields so consumerState() derives the right
// pill on the next render.
function applyTransitions(scope, id, transitions) {
  if (!Array.isArray(transitions) || transitions.length === 0) return;
  const cache = consumerCache.get(cacheKey(scope, id));
  if (!cache || !Array.isArray(cache.consumers)) return;
  for (const t of transitions) {
    const addonId = t.addonId || t.addon_id;
    if (!addonId) continue;
    const row = cache.consumers.find((c) => consumerAddonId(c) === addonId);
    if (!row) continue;
    applyStateToRow(row, String(t.after || '').toLowerCase());
  }
}

// Mutates a single cached consumer row to reflect a target grant state by
// rewriting the lifecycle timestamps consumerState() reads.
function applyStateToRow(row, state) {
  const now = new Date().toISOString();
  if (state === 'granted') {
    row.grantedAt = now;
    row.revokedAt = null;
    row.grant_status = 'granted';
  } else if (state === 'revoked' || state === 'denied') {
    row.revokedAt = now;
    row.grant_status = 'revoked';
  } else if (state === 'pending') {
    row.grantedAt = null;
    row.revokedAt = null;
    row.grant_status = 'pending';
  }
}

// Grant/revoke a single consumer. Optimistically flips the cached row, repaints,
// then reconciles with the response (which may itself carry transitions). On
// error the previous row snapshot is restored.
async function mutateConsumer(kind, scope, id, addonId, rerender) {
  if (!scope || !id || !addonId) return;
  const key = cacheKey(scope, id);
  const cache = consumerCache.get(key);
  const row = cache?.consumers?.find((c) => consumerAddonId(c) === addonId);
  const snapshot = row ? { ...row } : null;
  if (row) applyStateToRow(row, kind === 'grant' ? 'granted' : 'revoked');
  if (typeof rerender === 'function') rerender();
  const wire = WIRE[scope];
  try {
    const resp = await ApiBinary.action(kind === 'grant' ? wire.grant : wire.revoke, {
      [wire.idKey]: scopeId(scope, id),
      addonId,
    });
    applyTransitions(scope, id, resp?.transitions);
    toast(I18n.t(kind === 'grant' ? 'services.access_granted' : 'services.access_revoked'), 'success');
    if (typeof rerender === 'function') rerender();
  } catch (err) {
    // Revert optimistic mutation.
    if (row && snapshot) Object.assign(row, snapshot);
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    if (typeof rerender === 'function') rerender();
  }
}
