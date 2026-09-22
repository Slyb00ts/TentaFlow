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
//       `ProviderAccountBody` family. Signing an account in opens the A02
//       wizard; installing a CLI runs on the node the row names, so the matrix
//       polls itself until that node's own report settles.
// =============================================================================

import '/js/components/tf-table.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-menu.js';
import { escapeAttr, escapeHtml, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import {
  AgentAccounts,
  T,
  accountSubtitle,
  credentialKindLabel,
  engineName,
  engineTile,
  describeError,
  errorText,
  osLabel,
  scopeChipHtml,
  shortId,
  statusChipHtml,
  usedOnLabel,
} from '/js/modules/agent-accounts.js';
import { openAccountWindow, openCreateAccountWindow } from '/js/modules/agent-accounts-window.js';
import { openLoginWizard } from '/js/modules/agent-accounts-login.js';

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
  // The list's own error as `{ message, detail }` — the same shape the account
  // window renders: the translation (or the node's sentence) in `.aa-error`,
  // the node's raw English as that element's `title`, the way
  // `agent-accounts-window.js` keeps its inline errors.
  error: { message: '', detail: '' },
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
  state.error = { message: '', detail: '' };
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
    state.error = { message: '', detail: '' };
  } catch (err) {
    state.accounts = [];
    state.error = describeError(err);
  }
  if (state.segment === 'runtime' || state.isAdmin) {
    try {
      const runtime = await AgentAccounts.runtimeNodes();
      state.nodes = runtime.nodes ?? [];
    } catch {
      // A transport, decode or reconnect failure leaves the matrix unread. An
      // unread matrix must not render as "this account is homed here": with no
      // row marked as this machine the window cannot place the home node, so it
      // is handed an empty matrix and reports the scope as unknown instead of
      // claiming the account's list is the one this node holds.
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

// The catalog arrives with the first response, so the filter is filled through
// `setOptions` — a tf-select consumes its light-DOM options when it builds, and
// re-setting innerHTML afterwards would leave the built list untouched.
function paintEngineFilter() {
  const select = state.host?.querySelector('[data-field="engine"]');
  if (!select) return;
  const options = [{ value: '', label: T('filter_engine_all') }].concat(
    state.engines.map((engine) => {
      const id = engine.engine_id ?? engine.engineId;
      return { value: id, label: engineName(id, state.engines) };
    }),
  );
  select.setOptions(options, state.engineFilter);
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
    ${state.error.message
      ? `<p class="aa-error" role="alert" title="${escapeAttr(state.error.detail)}">${escapeHtml(state.error.message)}</p>`
      : ''}
    <!-- On a phone the name and the status are what a person scans for; the
         rest waits in the account detail one tap away. -->
    <tf-table variant="flush" id="aa-accounts-table" actions-label="${escapeAttr(I18n.t('common.actions'))}"
              empty-message="${escapeAttr(T('list_empty'))}">
      <tf-column key="account" label="${escapeAttr(T('col_account'))}" renderer="html" fill></tf-column>
      <tf-column key="scope" label="${escapeAttr(T('col_scope'))}" renderer="html" priority="low"></tf-column>
      <tf-column key="credential" label="${escapeAttr(T('col_credential'))}" priority="low"></tf-column>
      <tf-column key="status" label="${escapeAttr(T('col_status'))}" renderer="html"></tf-column>
      <tf-column key="sessions" label="${escapeAttr(T('col_sessions'))}" renderer="html" priority="low"></tf-column>
      <tf-column key="nodes" label="${escapeAttr(T('col_used_on'))}" priority="low"
                 hint="${escapeAttr(T('used_on_tooltip'))}"></tf-column>
    </tf-table>
    <div class="aa-foot" id="aa-accounts-foot"></div>`;

  const table = body.querySelector('#aa-accounts-table');
  table.rows = state.accounts.map((account) => ({
    account: `<div class="tf-table__ent">${engineTile(account.engine_id)}<div>`
      + `<div class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(account.display_name ?? '')}</div>`
      + `<div class="tf-table__cell-sub">${escapeHtml(accountSubtitle(account) || shortId(account.account_id))}</div>`
      + '</div></div>',
    scope: scopeChipHtml(account.scope),
    credential: credentialKindLabel(account.credential_kind),
    status: statusChipHtml(account),
    sessions: sessionsCell(account),
    // The nodes that hold this account's credential, as this node measured
    // them — the column header carries the same caveat as its tooltip.
    nodes: usedOnLabel(account),
    _accountId: account.account_id,
    _manageable: account.scope === 'global',
    // "Zaloguj" belongs where the account is waiting for one: a shared account
    // with no stored credential, or one the node put back into `needs_login`
    // after the bridge rejected what a session produced.
    _needsLogin: account.scope === 'global'
      && account.credential_kind === 'provider_login'
      && account.status !== 'disabled'
      && (Number(account.credential_revision ?? 0) === 0 || account.status === 'needs_login'),
    _engineId: account.engine_id,
    _name: account.display_name ?? '',
    _homeNodeId: account.home_node_id ?? null,
  }));
  table.rowActions = (row, _idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    if (!row._manageable) {
      const note = document.createElement('span');
      note.className = 'tf-table__cell-sub';
      note.textContent = T('owner_only');
      return note;
    }
    const actions = document.createElement('div');
    actions.className = 'tf-table__row-actions';
    if (row._needsLogin && state.isAdmin) {
      const login = document.createElement('tf-button');
      login.setAttribute('variant', 'primary');
      login.setAttribute('size', 'sm');
      login.textContent = T('action_login');
      login.addEventListener('click', (event) => {
        event.stopPropagation();
        const current = live();
        openLoginWizard({
          accountId: current._accountId,
          accountName: current._name,
          engineId: current._engineId,
          engines: state.engines,
          nodes: state.nodes,
          homeNodeId: current._homeNodeId,
          onFinished: () => load(),
        });
      });
      actions.appendChild(login);
    }
    const button = document.createElement('tf-button');
    button.setAttribute('variant', 'ghost');
    button.setAttribute('size', 'sm');
    button.textContent = T('action_open');
    button.addEventListener('click', (event) => {
      event.stopPropagation();
      openAccount(live()._accountId);
    });
    actions.appendChild(button);
    return actions;
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
    runtimeNodes: state.nodes,
    isAdmin: state.isAdmin,
    onChanged: () => load(),
  }).catch((err) => toast(errorText(err), 'error'));
}

// =============================================================================
// N01 — applications on nodes
// =============================================================================

function paintRuntime(body) {
  const engines = state.engines;
  body.innerHTML = `
    <tf-table variant="flush" id="aa-runtime-table" actions-label="${escapeAttr(T('col_receives'))}"
              empty-message="${escapeAttr(T('runtime_empty'))}">
      <tf-column key="node" label="${escapeAttr(T('col_node'))}" renderer="html" fill></tf-column>
      <tf-column key="sandbox" label="${escapeAttr(T('col_sandbox'))}" renderer="html"></tf-column>
      ${engines.map((engine) => {
        const id = engine.engine_id ?? engine.engineId;
        return `<tf-column key="engine_${escapeAttr(id)}" label="${escapeAttr(engineName(id, engines))}" renderer="html"></tf-column>`;
      }).join('')}
    </tf-table>
    <tf-menu placement="bottom-end" compact id="aa-runtime-menu">
      <tf-menu-item action="install" icon="download"></tf-menu-item>
      <tf-menu-item action="uninstall" icon="trash" danger></tf-menu-item>
    </tf-menu>
    <div class="aa-foot">
      <p>${escapeHtml(T('runtime_foot_install'))}</p>
      <p>${escapeHtml(T('runtime_foot_receives'))}</p>
    </div>`;

  const table = body.querySelector('#aa-runtime-table');
  wireRuntimeMenu(body);
  table.rows = state.nodes.map((node) => {
    const capable = node.sandbox_capable;
    const byEngine = new Map((node.engines ?? []).map((entry) => [entry.engine_id ?? entry.engineId, entry]));
    const gate = runtimeGate(node);
    const row = {
      // The sub-line says what the operator can act on: the operating system
      // this node reported (only a node knows its own), whether it is reachable
      // and how many accounts already have a credential on it.
      node: `<div class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(node.node_name ?? '')}</div>`
        + `<div class="tf-table__cell-sub">${escapeHtml(nodeSubLine(node))}</div>`
        + (gate.ok ? '' : `<div class="tf-table__cell-sub">${escapeHtml(T(gate.reason))}</div>`),
      sandbox: sandboxChip(capable),
      _nodeId: node.node_id,
      _receives: node.receives_accounts === true,
    };
    for (const engine of engines) {
      const id = engine.engine_id ?? engine.engineId;
      row[`engine_${id}`] = installCell(node, id, byEngine.get(id), gate);
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
        toast(errorText(err), 'error');
      }
    });
    return toggle;
  };
}

function nodeSubLine(node) {
  const count = Number(node.account_count ?? 0);
  const accounts = count > 0 ? T('node_accounts', { n: count }) : T('node_accounts_none');
  return `${osLabel(node.os)} · ${T(node.online ? 'node_online' : 'node_offline')} · ${accounts}`;
}

/**
 * Whether this node can be told to install or remove a CLI, and why not.
 *
 * Two answers only a node can give about itself: whether it can isolate a
 * process at all, and what it runs on. A peer reports neither until the matrix
 * is read ON it, so an unreachable node is refused here rather than through a
 * forwarded request that would time out with nothing to show for it.
 */
function runtimeGate(node) {
  if (node.sandbox_capable === false) return { ok: false, reason: 'runtime_not_possible' };
  if (!node.online) return { ok: false, reason: 'runtime_node_unreachable' };
  return { ok: true, reason: '' };
}

function sandboxChip(capable) {
  if (capable === true) return `<tf-chip size="sm" status="ok" dot label="${escapeAttr(T('sandbox_available'))}"></tf-chip>`;
  if (capable === false) return `<tf-chip size="sm" status="err" dot label="${escapeAttr(T('sandbox_missing'))}"></tf-chip>`;
  return `<tf-chip size="sm" status="neutral" label="${escapeAttr(T('sandbox_unknown'))}"></tf-chip>`;
}

/**
 * "5 · 3 osoby" as in the mockup: the people are named only when there is more
 * than one of them — "2 · 1 osoba" says nothing the reader did not assume.
 */
function sessionsCell(account) {
  const sessions = Number(account.session_count ?? 0);
  const people = Number(account.session_user_count ?? 0);
  return `<b>${sessions}</b>` + (people > 1
    ? ` <span class="tf-table__cell-sub">· ${escapeHtml(T('sessions_people', { count: people }))}</span>`
    : '');
}

/**
 * One engine cell of the matrix: the state the owning node reports, plus the
 * menu that installs or removes it there.
 *
 * The button is markup rather than an element, because a cell is written as
 * HTML into the table's shadow root; the menu it opens lives OUTSIDE that root
 * (one per matrix), so its panel is not clipped by the table and the click that
 * opened it is what tells the menu which node and engine it is acting on.
 */
function installCell(node, engineId, entry, gate) {
  const stateName = entry?.install_state ?? entry?.installState ?? 'absent';
  // Nothing installed means one possible action, so it is the cell itself.
  if (stateName === 'absent') {
    return `<tf-button variant="secondary" size="sm" icon="download"
      data-runtime-install data-node="${escapeAttr(node.node_id ?? '')}" data-engine="${escapeAttr(engineId)}"
      ${gate.ok ? '' : `disabled title="${escapeAttr(T(gate.reason))}"`}>${escapeHtml(T('runtime_install'))}</tf-button>`;
  }
  const trigger = `<tf-button variant="ghost" size="sm" icon="more"
      data-runtime-menu data-node="${escapeAttr(node.node_id ?? '')}" data-engine="${escapeAttr(engineId)}"
      data-state="${escapeAttr(stateName)}"
      aria-label="${escapeAttr(T('runtime_actions'))}"
      ${gate.ok ? '' : `disabled title="${escapeAttr(T(gate.reason))}"`}></tf-button>`;
  let label;
  if (stateName === 'installed') {
    const version = entry.version;
    label = version
      ? `<span class="tf-table__cell-title">${escapeHtml(version)}</span>`
      : `<tf-chip size="sm" status="ok" dot label="${escapeAttr(T('install_installed'))}"></tf-chip>`;
  } else if (stateName === 'installing') {
    label = `<tf-chip size="sm" status="info" dot label="${escapeAttr(T('install_installing'))}"></tf-chip>`;
  } else if (stateName === 'error') {
    const reason = entry.last_error ?? entry.lastError ?? '';
    label = `<tf-chip size="sm" status="err" dot label="${escapeAttr(T('install_error'))}"></tf-chip>`
      + (reason ? `<div class="tf-table__cell-sub">${escapeHtml(reason)}</div>` : '');
  }
  // `tf-table__ent` is the shared "icon plus text on one line" cell layout from
  // controls.css — the only sheet a shadow root adopts, so this is what a cell
  // may use; a class from agent-accounts.css would never reach it.
  return `<div class="tf-table__ent">${label}${trigger}</div>`;
}

/**
 * One menu for the whole matrix, opened from the cell that was clicked.
 *
 * A click inside a shadow root is `composed`, so it reaches this listener with
 * `composedPath()` naming the button it started on — which is how a cell
 * written as HTML gets a real, anchored `tf-menu` without every cell building
 * one of its own.
 */
function wireRuntimeMenu(body) {
  const menu = body.querySelector('#aa-runtime-menu');
  const install = menu.querySelector('[action="install"]');
  const uninstall = menu.querySelector('[action="uninstall"]');
  let target = null;

  body.querySelector('#aa-runtime-table').addEventListener('click', (event) => {
    const direct = event.composedPath().find((el) => el?.dataset?.runtimeInstall !== undefined);
    if (direct) {
      if (!direct.hasAttribute('disabled')) runInstall(direct.dataset.node, direct.dataset.engine, true);
      return;
    }
    const trigger = event.composedPath().find((el) => el?.dataset?.runtimeMenu !== undefined);
    if (!trigger || trigger.hasAttribute('disabled')) return;
    event.stopPropagation();
    target = { nodeId: trigger.dataset.node, engineId: trigger.dataset.engine, state: trigger.dataset.state };
    const installed = target.state === 'installed';
    install.setAttribute('label', T(installed ? 'runtime_reinstall' : 'runtime_install'));
    uninstall.setAttribute('label', T('runtime_uninstall'));
    menu.anchor = trigger;
    menu.open();
  });

  menu.addEventListener('action', (event) => {
    const action = event.detail?.action;
    if (!target || (action !== 'install' && action !== 'uninstall')) return;
    runInstall(target.nodeId, target.engineId, action === 'install');
  });
}

/**
 * Installs or removes one engine and then follows the node's own report.
 *
 * The request answers with the matrix row the NODE now holds, not with what was
 * asked for; a state that is still `installing` when it lands means the node is
 * still working, so the matrix keeps reading until it settles instead of
 * leaving a chip that never changes.
 */
async function runInstall(nodeId, engineId, install) {
  const label = engineName(engineId, state.engines);
  toast(T(install ? 'runtime_install_started' : 'runtime_uninstall_started', { engine: label }), 'info');
  try {
    await AgentAccounts[install ? 'installRuntime' : 'uninstallRuntime'](nodeId, engineId);
    toast(T(install ? 'runtime_installed' : 'runtime_uninstalled', { engine: label }), 'success');
  } catch (err) {
    toast(errorText(err), 'error');
  }
  await load();
  await watchInstall(nodeId, engineId);
}

/** Re-reads the matrix while one cell is still `installing`, and then stops. */
async function watchInstall(nodeId, engineId) {
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const node = state.nodes.find((row) => row.node_id === nodeId);
    const entry = (node?.engines ?? []).find((e) => (e.engine_id ?? e.engineId) === engineId);
    if ((entry?.install_state ?? entry?.installState ?? 'absent') !== 'installing') return;
    await new Promise((resolve) => setTimeout(resolve, 2000));
    if (!state.host || state.segment !== 'runtime') return;
    await load();
  }
}
