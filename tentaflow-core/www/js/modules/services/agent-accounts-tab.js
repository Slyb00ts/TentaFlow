// =============================================================================
// File: modules/services/agent-accounts-tab.js — the "Konta agentów" tab of the
//       Services screen: the account list (A01) and the node × application
//       matrix (N01) behind one segmented control.
//
//       The tab owns its host element: Services renders an empty div and this
//       module fills it, so the 5 s service refresher never patches over a
//       toolbar the operator is typing in.
//
//       Both halves are administration, and both read the binary
//       `ProviderAccountBody` family. Signing an account in (A02) and
//       installing a CLI on a node are refused by this node today, so the
//       affordances for them are absent rather than dead.
// =============================================================================

import '/js/components/tf-table.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-toggle.js';
import { escapeAttr, escapeHtml, toast } from '/js/utils.js';
import {
  AgentAccounts,
  T,
  accountSubtitle,
  credentialKindLabel,
  engineName,
  engineTile,
  scopeChipHtml,
  shortId,
  statusChipHtml,
} from '/js/modules/agent-accounts.js';
import { openAccountWindow, openCreateAccountWindow } from '/js/modules/agent-accounts-window.js';

const state = {
  host: null,
  isAdmin: false,
  onCount: null,
  segment: 'accounts',
  engineFilter: '',
  scopeFilter: '',
  query: '',
  accounts: [],
  engines: [],
  nodes: [],
  error: '',
};

/** Renders the tab into `host` and loads it. Re-entrant: Services re-mounts on every tab switch. */
export function mount(host, { isAdmin = false, onCount = null } = {}) {
  state.host = host;
  state.isAdmin = isAdmin;
  state.onCount = onCount;
  host.innerHTML = `
    <div class="aa-tab">
      <div class="tf-toolbar" data-toolbar>
        <tf-segmented data-field="segment" size="md" value="${escapeAttr(state.segment)}">
          <option value="accounts">${escapeHtml(T('segment_accounts'))}</option>
          <option value="runtime">${escapeHtml(T('segment_runtime'))}</option>
        </tf-segmented>
        <tf-searchbox data-field="query" debounce="150"
          placeholder="${escapeAttr(T('search_placeholder'))}" value="${escapeAttr(state.query)}"></tf-searchbox>
        <tf-select data-field="engine" aria-label="${escapeAttr(T('filter_engine'))}"></tf-select>
        <tf-select data-field="scope" aria-label="${escapeAttr(T('filter_scope'))}">
          <option value="">${escapeHtml(T('filter_scope_all'))}</option>
          <option value="global">${escapeHtml(T('scope_global'))}</option>
          <option value="user">${escapeHtml(T('scope_user'))}</option>
        </tf-select>
        <span class="tf-toolbar-spacer"></span>
        <tf-button variant="primary" icon="plus" data-act="create">${escapeHtml(T('action_create'))}</tf-button>
      </div>
      <div data-segment-body></div>
    </div>`;

  const field = (name) => host.querySelector(`[data-field="${name}"]`);
  field('segment').addEventListener('change', (event) => {
    state.segment = event.detail?.value || 'accounts';
    paintToolbar();
    paint();
    if (state.segment === 'runtime' && !state.nodes.length) load();
  });
  field('query').addEventListener('search', (event) => {
    state.query = String(event.detail?.value || '').trim();
    load();
  });
  field('engine').addEventListener('change', (event) => {
    state.engineFilter = event.currentTarget.value || '';
    load();
  });
  field('scope').addEventListener('change', (event) => {
    state.scopeFilter = event.currentTarget.value || '';
    load();
  });
  host.querySelector('[data-act="create"]').addEventListener('click', () => {
    openCreateAccountWindow({
      engines: state.engines,
      scope: 'global',
      onCreated: (accountId) => {
        load().then(() => {
          if (accountId) openAccount(accountId);
        });
      },
    });
  });

  paintToolbar();
  return load();
}

export function unmount() {
  state.host = null;
  state.accounts = [];
  state.engines = [];
  state.nodes = [];
  state.error = '';
}

async function load() {
  if (!state.host) return;
  try {
    const list = await AgentAccounts.list({
      engineId: state.engineFilter || null,
      scope: state.scopeFilter || null,
      query: state.query || null,
    });
    state.accounts = list.accounts ?? [];
    state.engines = list.engines ?? [];
    state.error = '';
  } catch (err) {
    state.accounts = [];
    state.error = err.message || String(err);
  }
  if (state.segment === 'runtime' || state.isAdmin) {
    try {
      const runtime = await AgentAccounts.runtimeNodes();
      state.nodes = runtime.nodes ?? [];
    } catch {
      // The matrix is administration-only; a refusal here must not blank the
      // account list a non-administrator is allowed to see.
      state.nodes = [];
    }
  }
  if (!state.host) return;
  state.onCount?.(state.accounts.length);
  paintEngineFilter();
  paint();
}

function paintToolbar() {
  const host = state.host;
  if (!host) return;
  const accounts = state.segment === 'accounts';
  for (const name of ['query', 'engine', 'scope']) {
    host.querySelector(`[data-field="${name}"]`).hidden = !accounts;
  }
  host.querySelector('[data-act="create"]').hidden = !accounts || !state.isAdmin;
}

function paintEngineFilter() {
  const select = state.host?.querySelector('[data-field="engine"]');
  if (!select) return;
  const options = [`<option value="">${escapeHtml(T('filter_engine_all'))}</option>`]
    .concat(state.engines.map((engine) => {
      const id = engine.engine_id ?? engine.engineId;
      return `<option value="${escapeAttr(id)}">${escapeHtml(engineName(id, state.engines))}</option>`;
    }));
  select.innerHTML = options.join('');
  select.value = state.engineFilter;
}

function paint() {
  const body = state.host?.querySelector('[data-segment-body]');
  if (!body) return;
  if (state.segment === 'runtime') paintRuntime(body);
  else paintAccounts(body);
}

// =============================================================================
// A01 — accounts
// =============================================================================

function paintAccounts(body) {
  body.innerHTML = `
    ${state.error ? `<p class="aa-error" role="alert">${escapeHtml(state.error)}</p>` : ''}
    <tf-table variant="flush" id="aa-accounts-table" empty-message="${escapeAttr(T('list_empty'))}">
      <tf-column key="account" label="${escapeAttr(T('col_account'))}" fill></tf-column>
      <tf-column key="scope" label="${escapeAttr(T('col_scope'))}"></tf-column>
      <tf-column key="credential" label="${escapeAttr(T('col_credential'))}"></tf-column>
      <tf-column key="status" label="${escapeAttr(T('col_status'))}"></tf-column>
      <tf-column key="sessions" label="${escapeAttr(T('col_sessions'))}"></tf-column>
      <tf-column key="nodes" label="${escapeAttr(T('col_used_on'))}"></tf-column>
    </tf-table>
    <div class="aa-foot" id="aa-accounts-foot"></div>`;

  const table = body.querySelector('#aa-accounts-table');
  table.rows = state.accounts.map((account) => ({
    account: `<div class="tf-table__ent">${engineTile(account.engine_id)}<div>`
      + `<div class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(account.display_name ?? '')}</div>`
      + `<div class="tf-table__cell-sub">${escapeHtml(accountSubtitle(account) || shortId(account.account_id))}</div>`
      + '</div></div>',
    scope: scopeChipHtml(account.scope),
    credential: escapeHtml(credentialKindLabel(account.credential_kind)),
    status: statusChipHtml(account),
    sessions: `<b>${Number(account.session_count ?? 0)}</b>`,
    // The wire knows which node HOLDS the login; which nodes have materialized
    // it is per-account state and lives in the account window, not in a list
    // that would need one request per row to fill this cell.
    nodes: escapeHtml(account.home_node_name ?? T('value_none')),
    _accountId: account.account_id,
    _manageable: account.scope === 'global',
  }));
  table.rowActions = (row, _idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    if (!row._manageable) {
      const note = document.createElement('span');
      note.className = 'tf-table__cell-sub';
      note.textContent = T('owner_only');
      return note;
    }
    const button = document.createElement('tf-button');
    button.setAttribute('variant', 'ghost');
    button.setAttribute('size', 'sm');
    button.textContent = T('action_open');
    button.addEventListener('click', (event) => {
      event.stopPropagation();
      openAccount(live()._accountId);
    });
    return button;
  };
  table.addEventListener('row-click', (event) => {
    const row = event.detail?.row;
    if (row?._manageable) openAccount(row._accountId);
  });

  const total = state.accounts.length;
  const global = state.accounts.filter((a) => a.scope === 'global').length;
  body.querySelector('#aa-accounts-foot').textContent = T('list_foot', {
    total, global, personal: total - global,
  });
}

function openAccount(accountId) {
  openAccountWindow(accountId, {
    engines: state.engines,
    isAdmin: state.isAdmin,
    onChanged: () => load(),
  }).catch((err) => toast(err.message || String(err), 'error'));
}

// =============================================================================
// N01 — applications on nodes
// =============================================================================

function paintRuntime(body) {
  const engines = state.engines;
  body.innerHTML = `
    <tf-table variant="flush" id="aa-runtime-table" empty-message="${escapeAttr(T('runtime_empty'))}">
      <tf-column key="node" label="${escapeAttr(T('col_node'))}" fill></tf-column>
      <tf-column key="sandbox" label="${escapeAttr(T('col_sandbox'))}"></tf-column>
      ${engines.map((engine) => {
        const id = engine.engine_id ?? engine.engineId;
        return `<tf-column key="engine_${escapeAttr(id)}" label="${escapeAttr(engineName(id, engines))}"></tf-column>`;
      }).join('')}
      <tf-column key="receives" label="${escapeAttr(T('col_receives'))}"></tf-column>
    </tf-table>
    <div class="aa-foot">
      <p>${escapeHtml(T('runtime_foot_install'))}</p>
      <p>${escapeHtml(T('runtime_foot_receives'))}</p>
    </div>`;

  const table = body.querySelector('#aa-runtime-table');
  table.rows = state.nodes.map((node) => {
    const capable = node.sandbox_capable;
    const byEngine = new Map((node.engines ?? []).map((entry) => [entry.engine_id ?? entry.engineId, entry]));
    const row = {
      node: `<div class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(node.node_name ?? '')}</div>`
        + `<div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(shortId(node.node_id))}</div>`
        + (capable === false ? `<div class="tf-table__cell-sub">${escapeHtml(T('runtime_not_possible'))}</div>` : ''),
      sandbox: sandboxChip(capable),
      receives: '',
      _nodeId: node.node_id,
      _receives: node.receives_accounts === true,
    };
    for (const engine of engines) {
      const id = engine.engine_id ?? engine.engineId;
      row[`engine_${id}`] = installCell(byEngine.get(id));
    }
    return row;
  });
  table.rowActions = (row, _idx, currentRow) => {
    const toggle = document.createElement('tf-toggle');
    toggle.toggleAttribute('checked', row._receives);
    toggle.setAttribute('aria-label', T('col_receives'));
    toggle.addEventListener('change', async () => {
      const live = currentRow?.() ?? row;
      const enabled = toggle.hasAttribute('checked');
      try {
        await AgentAccounts.setReceivesAccounts(live._nodeId, enabled);
        live._receives = enabled;
        toast(T(enabled ? 'receives_on' : 'receives_off'), 'success');
        await load();
      } catch (err) {
        toggle.toggleAttribute('checked', !enabled);
        toast(err.message || String(err), 'error');
      }
    });
    return toggle;
  };
}

function sandboxChip(capable) {
  if (capable === true) return `<tf-chip size="sm" status="ok" dot label="${escapeAttr(T('sandbox_available'))}"></tf-chip>`;
  if (capable === false) return `<tf-chip size="sm" status="err" dot label="${escapeAttr(T('sandbox_missing'))}"></tf-chip>`;
  return `<tf-chip size="sm" status="neutral" label="${escapeAttr(T('sandbox_unknown'))}"></tf-chip>`;
}

/**
 * One engine cell of the matrix. Read-only on purpose: installing a CLI is a
 * node-side operation this build refuses, and a dead "Zainstaluj" button would
 * be a promise the node cannot keep.
 */
function installCell(entry) {
  const stateName = entry?.install_state ?? entry?.installState ?? 'absent';
  if (stateName === 'installed') {
    const version = entry.version;
    return version
      ? `<span class="tf-table__cell-title">${escapeHtml(version)}</span>`
      : `<tf-chip size="sm" status="ok" dot label="${escapeAttr(T('install_installed'))}"></tf-chip>`;
  }
  if (stateName === 'installing') {
    return `<tf-chip size="sm" status="info" dot label="${escapeAttr(T('install_installing'))}"></tf-chip>`;
  }
  if (stateName === 'error') {
    const reason = entry.last_error ?? entry.lastError ?? '';
    return `<tf-chip size="sm" status="err" dot label="${escapeAttr(T('install_error'))}"></tf-chip>`
      + (reason ? `<div class="tf-table__cell-sub">${escapeHtml(reason)}</div>` : '');
  }
  return `<span class="tf-table__cell-sub">${escapeHtml(T('install_absent'))}</span>`;
}
