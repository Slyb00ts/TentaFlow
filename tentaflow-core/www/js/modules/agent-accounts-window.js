// =============================================================================
// File: modules/agent-accounts-window.js — the two windows of an agent provider
//       account: "new account" (A01 → Dodaj konto, U01 → Połącz) and the account
//       detail with its Przegląd / Dostęp / Agenci tabs (A03/A04).
//
//       Both read and write ONLY through the `ProviderAccountBody` family
//       (modules/agent-accounts.js). Signing the account in opens the A02
//       wizard (modules/agent-accounts-login.js), which runs the vendor CLI on
//       a node — this window never touches credential material itself.
// =============================================================================

import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-combobox.js';
import { TfWindow } from '/js/components/tf-window.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { escapeAttr, escapeHtml, toast } from '/js/utils.js';
import {
  AgentAccounts,
  T,
  accountSubtitle,
  credentialKindLabel,
  credentialKindsFor,
  engineEntry,
  engineName,
  engineTile,
  describeError,
  errorText,
  grantedByLabel,
  nodeCredentialLabel,
  shortId,
  sinceLabel,
  statusChipHtml,
  usedOnLabel,
  whenLabel,
} from '/js/modules/agent-accounts.js';
import { openLoginWizard } from '/js/modules/agent-accounts-login.js';

// =============================================================================
// New account
// =============================================================================

/**
 * Creates an account and, for an `api_key` engine, stores its key in the same
 * step. `scope` is fixed by the caller: A01 creates shared accounts, U01
 * creates the caller's own — the server mints a personal account for whoever
 * asks, so the window never offers to pick another owner.
 */
export function openCreateAccountWindow({ engines = [], scope = 'global', onCreated = null } = {}) {
  const available = engines.filter((engine) => credentialKindsFor(engine).length > 0);
  if (!available.length) {
    toast(T('create_no_engines'), 'warning');
    return;
  }
  const engineOptions = available
    .map((engine) => {
      const id = engine.engine_id ?? engine.engineId;
      return `<option value="${escapeAttr(id)}">${escapeHtml(engineName(id, engines))}</option>`;
    })
    .join('');

  const body = document.createElement('div');
  body.className = 'aa-form';
  body.innerHTML = `
    <tf-select data-field="engine" label="${escapeAttr(T('field_engine'))}">${engineOptions}</tf-select>
    <tf-input data-field="name" label="${escapeAttr(T('field_name'))}"
      placeholder="${escapeAttr(T('field_name_placeholder'))}" maxlength="120"></tf-input>
    <div>
      <tf-segmented data-field="kind" size="md"></tf-segmented>
      <div class="aa-hint" data-kind-hint></div>
    </div>
    <div data-key-field hidden>
      <tf-input data-field="key" type="password" label="${escapeAttr(T('field_api_key'))}"
        autocomplete="off" hint="${escapeAttr(T('field_api_key_hint'))}"></tf-input>
    </div>
    <p class="aa-note" data-login-note hidden>${escapeHtml(T('create_login_note'))}</p>
    <p class="aa-error" role="alert" data-error hidden></p>`;
  const footer = document.createElement('div');
  footer.innerHTML = `
    <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
    <tf-button variant="primary" data-act="create">${escapeHtml(T('create_submit'))}</tf-button>`;

  // `TfWindow.open` resolves only when the window CLOSES and never hands back
  // the element, so the handle comes from the body node it was given — which is
  // in the DOM by the time the call returns.
  TfWindow.open({
    title: T(scope === 'user' ? 'create_title_own' : 'create_title'),
    subtitle: T(scope === 'user' ? 'create_sub_own' : 'create_sub'),
    icon: 'users',
    width: 560,
    modal: true,
    buttons: 'close',
    body,
    footer,
  });
  const host = body.closest('tf-window');
  if (!host) return;
  const field = (name) => body.querySelector(`[data-field="${name}"]`);
  const error = body.querySelector('[data-error]');
  const kindSegment = field('kind');

  // Which methods exist depends on the engine, so the segmented control is
  // filled through `setOptions`: its light-DOM options are consumed when it
  // builds and re-setting innerHTML would not reach the rendered buttons.
  const paintKinds = () => {
    const engine = engineEntry(engines, field('engine').value);
    const kinds = credentialKindsFor(engine);
    kindSegment.setOptions(
      kinds.map((kind) => ({ value: kind, label: credentialKindLabel(kind) })),
      kindSegment.value,
    );
    paintKindDetails();
  };
  const paintKindDetails = () => {
    const apiKey = kindSegment.value === 'api_key';
    body.querySelector('[data-key-field]').hidden = !apiKey;
    body.querySelector('[data-login-note]').hidden = apiKey;
    body.querySelector('[data-kind-hint]').textContent = T(apiKey ? 'kind_hint_api_key' : 'kind_hint_login');
  };

  field('engine').addEventListener('change', paintKinds);
  kindSegment.addEventListener('change', paintKindDetails);
  paintKinds();

  footer.querySelector('[data-act="create"]').addEventListener('click', async (event) => {
    const button = event.currentTarget;
    const displayName = String(field('name').value || '').trim();
    error.hidden = true;
    if (!displayName) {
      error.textContent = T('err_name_required');
      error.hidden = false;
      return;
    }
    const credentialKind = kindSegment.value;
    const material = String(field('key').value || '').trim();
    if (credentialKind === 'api_key' && !material) {
      error.textContent = T('err_key_required');
      error.hidden = false;
      return;
    }
    button.setAttribute('disabled', '');
    try {
      const created = await AgentAccounts.create({
        engineId: field('engine').value,
        displayName,
        scope,
        credentialKind,
      });
      const accountId = created?.account?.account_id ?? created?.account?.accountId;
      if (credentialKind === 'api_key' && accountId) {
        await AgentAccounts.setCredential(accountId, material);
      }
      toast(T('create_ok', { name: displayName }), 'success');
      host.close();
      onCreated?.(accountId ?? null);
    } catch (err) {
      const refusal = describeError(err);
      error.textContent = refusal.message;
      error.title = refusal.detail;
      error.hidden = false;
    } finally {
      button.removeAttribute('disabled');
    }
  });
}

// =============================================================================
// Account detail (A03/A04)
// =============================================================================

/**
 * Opens the account window. `isAdmin` decides whether the Dostęp tab and the
 * status switch exist at all: granting access and disabling an account are
 * administrator decisions, and the server refuses them anyway — hiding them is
 * what keeps the window honest rather than optimistic.
 */
export async function openAccountWindow(accountId, {
  engines = [], runtimeNodes = [], isAdmin = false, onChanged = null,
} = {}) {
  const loaded = await AgentAccounts.get(accountId);
  const state = {
    account: loaded.account ?? {},
    grants: loaded.grants ?? [],
    sessions: loaded.sessions ?? [],
    nodes: loaded.nodes ?? [],
    agents: loaded.agents ?? [],
    // Access draft: the org switch and the picked subjects are one list on the
    // wire (a full replace), but two controls here.
    orgWide: (loaded.grants ?? []).some((g) => (g.subject_type ?? g.subjectType) === 'org'),
    subjects: (loaded.grants ?? []).filter((g) => (g.subject_type ?? g.subjectType) !== 'org'),
    accessDirty: false,
    users: [],
    groups: [],
    directoryLoaded: false,
    tab: 'overview',
  };
  const scopeUser = state.account.scope === 'user';
  const canAccess = isAdmin && !scopeUser;
  // Signing in decides which provider identity the account's runs are
  // attributed to, and the node refuses that on a PERSONAL account from anyone
  // but its owner — who has their own button in "Moje konta". A window an
  // administrator opened therefore offers the sign-in on shared accounts only.
  const canLogin = isAdmin && !scopeUser && state.account.credential_kind === 'provider_login';

  const body = document.createElement('div');
  body.className = 'aa-detail';
  body.innerHTML = `
    <tf-tabs variant="underline" value="overview" data-tabs>
      <tf-tab id="overview" icon="info">${escapeHtml(T('tab_overview'))}</tf-tab>
      ${canAccess ? `<tf-tab id="access" icon="users" count="${state.grants.length}">${escapeHtml(T('tab_access'))}</tf-tab>` : ''}
      <tf-tab id="agents" icon="bot" count="${state.agents.length}">${escapeHtml(T('tab_agents'))}</tf-tab>
    </tf-tabs>
    <div data-pane></div>`;
  const footer = document.createElement('div');
  footer.innerHTML = `
    <tf-button variant="danger" data-act="delete" class="aa-footer-left">${escapeHtml(T('action_delete'))}</tf-button>
    ${canLogin ? `<tf-button variant="secondary" data-act="login"></tf-button>` : ''}
    <tf-button variant="primary" data-action="close">${escapeHtml(I18n.t('common.close'))}</tf-button>`;

  TfWindow.open({
    title: T('title'),
    subtitle: `${state.account.display_name ?? ''} · ${T(scopeUser ? 'subtitle_user' : 'subtitle_global')}`,
    icon: 'users',
    width: 900,
    modal: true,
    buttons: 'close',
    body,
    footer,
  });

  const host = body.closest('tf-window');
  if (!host) return;
  const pane = body.querySelector('[data-pane]');

  // Every write refreshes the caller straight away, so the list behind the
  // window never shows a name or a status this window has already changed.
  const notifyChanged = () => onChanged?.();

  const reload = async () => {
    const fresh = await AgentAccounts.get(accountId).catch(() => null);
    if (!fresh || !host.isConnected) return;
    state.account = fresh.account ?? state.account;
    state.grants = fresh.grants ?? [];
    state.sessions = fresh.sessions ?? [];
    state.nodes = fresh.nodes ?? [];
    state.agents = fresh.agents ?? [];
    if (!state.accessDirty) {
      state.orgWide = state.grants.some((g) => (g.subject_type ?? g.subjectType) === 'org');
      state.subjects = state.grants.filter((g) => (g.subject_type ?? g.subjectType) !== 'org');
    }
    body.querySelector('tf-tab#access')?.setAttribute('count', String(state.grants.length));
    body.querySelector('tf-tab#agents')?.setAttribute('count', String(state.agents.length));
    paint();
  };

  // A02 — the footer's sign-in, whose wording is the whole difference between a
  // first login and a repeat: an account that already holds a credential keeps
  // working until the new one lands.
  const loginButton = footer.querySelector('[data-act="login"]');
  loginButton?.addEventListener('click', () => {
    openLoginWizard({
      accountId,
      accountName: state.account.display_name ?? '',
      engineId: state.account.engine_id,
      engines,
      nodes: runtimeNodes,
      homeNodeId: state.account.home_node_id ?? null,
      onFinished: (outcome) => {
        if (outcome === 'succeeded') notifyChanged();
        reload();
      },
    });
  });

  const paint = () => {
    if (loginButton) {
      const revision = Number(state.account.credential_revision ?? 0);
      loginButton.setAttribute('label', T(revision > 0 ? 'action_relogin' : 'action_login'));
    }
    if (state.tab === 'access') paintAccess();
    else if (state.tab === 'agents') paintAgents();
    else paintOverview();
  };

  // ---- Przegląd ----------------------------------------------------------
  function paintOverview() {
    const account = state.account;
    const apiKey = account.credential_kind === 'api_key';
    const revision = Number(account.credential_revision ?? 0);
    const enabled = account.status !== 'disabled';
    pane.innerHTML = `
      <dl class="aa-kv">
        <dt>${escapeHtml(T('kv_provider_account'))}</dt>
        <dd>${escapeHtml(accountSubtitle(account) || '—')}</dd>
        <dt>${escapeHtml(T('kv_credential'))}</dt>
        <dd>${escapeHtml(credentialKindLabel(account.credential_kind))} · ${escapeHtml(revision > 0
          ? T('kv_revision', { n: revision, when: whenLabel(account.updated_at) })
          : T('kv_no_credential'))}</dd>
        <dt>${escapeHtml(T('kv_status'))}</dt>
        <dd>${statusChipHtml(account)}${account.status === 'needs_login'
          ? `<span class="aa-hint">${escapeHtml(T(apiKey ? 'kv_needs_key_hint' : 'kv_needs_login_hint'))}</span>`
          : ''}</dd>
        <dt>${escapeHtml(T('kv_engine'))}</dt>
        <dd>${engineTile(account.engine_id)} ${escapeHtml(engineName(account.engine_id, engines))}</dd>
        <dt>${escapeHtml(T('col_used_on'))}</dt>
        <dd title="${escapeAttr(T('used_on_tooltip'))}">${escapeHtml(usedOnLabel(account))}</dd>
      </dl>

      <section class="aa-section">
        <h4 class="aa-sub-h">${escapeHtml(T('settings_title'))}</h4>
        <div class="aa-form aa-form-row">
          <tf-input data-field="name" label="${escapeAttr(T('field_name'))}"
            value="${escapeAttr(account.display_name ?? '')}" maxlength="120"></tf-input>
          <tf-button variant="primary" data-act="rename">${escapeHtml(T('action_rename'))}</tf-button>
        </div>
        ${isAdmin ? `
          <div class="aa-toggle-row">
            <tf-toggle data-field="enabled" ${enabled ? 'checked' : ''}></tf-toggle>
            <div>
              <div class="aa-toggle-name">${escapeHtml(T('field_enabled'))}</div>
              <div class="aa-hint">${escapeHtml(T('field_enabled_hint'))}</div>
            </div>
          </div>` : ''}
        ${apiKey ? `
          <div class="aa-form aa-form-row">
            <tf-input data-field="key" type="password" label="${escapeAttr(T('field_api_key_replace'))}"
              autocomplete="off" placeholder="${escapeAttr(T('field_api_key_placeholder'))}"></tf-input>
            <tf-button variant="secondary" data-act="key-save">${escapeHtml(T('action_key_save'))}</tf-button>
            <tf-button variant="ghost" data-act="key-clear" ${revision > 0 ? '' : 'disabled'}>${escapeHtml(T('action_key_clear'))}</tf-button>
          </div>` : ''}
        <p class="aa-error" role="alert" data-error hidden></p>
      </section>

      <section class="aa-section">
        <h4 class="aa-sub-h">${escapeHtml(T('sessions_title', { count: state.sessions.length }))}</h4>
        <p class="aa-hint">${escapeHtml(T('sessions_hint'))}</p>
        <tf-table variant="flush" data-table="sessions" actions-label="${escapeAttr(I18n.t('common.actions'))}"
                  empty-message="${escapeAttr(T('sessions_empty'))}">
          <tf-column key="user" label="${escapeAttr(T('col_user'))}" renderer="html" fill></tf-column>
          <tf-column key="agent" label="${escapeAttr(T('col_agent'))}"></tf-column>
          <tf-column key="workspace" label="${escapeAttr(T('col_workspace'))}"></tf-column>
          <tf-column key="node" label="${escapeAttr(T('col_node'))}"></tf-column>
          <tf-column key="since" label="${escapeAttr(T('col_since'))}"></tf-column>
        </tf-table>
      </section>

      <section class="aa-section">
        <h4 class="aa-sub-h">${escapeHtml(T('nodes_title'))}</h4>
        <tf-table variant="flush" data-table="nodes" empty-message="${escapeAttr(T('nodes_empty'))}">
          <tf-column key="node" label="${escapeAttr(T('col_node'))}" renderer="html" fill></tf-column>
          <tf-column key="credential" label="${escapeAttr(T('col_node_credential'))}" renderer="html"></tf-column>
          <tf-column key="sessions" label="${escapeAttr(T('col_node_sessions'))}"></tf-column>
        </tf-table>
      </section>`;

    const sessionsByNode = new Map();
    for (const session of state.sessions) {
      const id = session.node_id ?? session.nodeId ?? '';
      sessionsByNode.set(id, (sessionsByNode.get(id) ?? 0) + 1);
    }
    const sessionsTable = pane.querySelector('[data-table="sessions"]');
    sessionsTable.rows = state.sessions.map((session) => ({
      user: `<div class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(session.user_display_name ?? '')}</div>`
        + `<div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(shortId(session.session_id))}</div>`,
      agent: session.agent_name ?? T('value_none'),
      workspace: session.workspace_name ?? T('value_none'),
      node: session.node_name ?? '',
      since: sinceLabel(session.started_at),
      _sessionId: session.session_id,
    }));
    // "Zakończ": the node closes the CLI first and only then forgets the row,
    // so a bridge that refuses leaves the session visible and closable instead
    // of disappearing from a list while its process keeps running.
    sessionsTable.rowActions = (row, _idx, currentRow) => {
      const button = document.createElement('tf-button');
      button.setAttribute('variant', 'danger');
      button.setAttribute('size', 'sm');
      button.textContent = T('action_end_session');
      button.addEventListener('click', async () => {
        const live = currentRow?.() ?? row;
        const confirmed = await TfWindow.confirm({
          title: T('session_end_confirm_title'),
          message: T('session_end_confirm_body'),
          confirmLabel: T('action_end_session'),
          cancelLabel: I18n.t('common.cancel'),
          danger: true,
        });
        if (!confirmed || !host.isConnected) return;
        button.setAttribute('disabled', '');
        try {
          await AgentAccounts.revokeSession(accountId, live._sessionId);
          toast(T('session_ended'), 'success');
          notifyChanged();
          await reload();
        } catch (err) {
          fail(err);
        } finally {
          reenable(button);
        }
      });
      return button;
    };
    pane.querySelector('[data-table="nodes"]').rows = state.nodes.map((node) => {
      const credential = nodeCredentialLabel(node, state.account.credential_revision);
      return {
        node: `<div class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(node.node_name ?? '')}</div>`
          + `<div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(shortId(node.node_id))}</div>`,
        credential: `<tf-chip size="sm" status="${credential.tone}" dot label="${escapeAttr(credential.label)}"></tf-chip>`
          + (node.last_error ? `<div class="tf-table__cell-sub">${escapeHtml(node.last_error)}</div>` : ''),
        sessions: String(sessionsByNode.get(node.node_id ?? '') ?? 0),
      };
    });

    const error = pane.querySelector('[data-error]');
    // A refusal has to be both readable where the operator is looking and
    // impossible to miss after the pane scrolled — the server's own message,
    // never a generic one, because it is what says WHY the write was refused.
    const fail = (err) => {
      const { message, detail } = describeError(err);
      error.textContent = message;
      // The node's own sentence stays reachable next to its translation; the
      // toast can only carry one line, the error line can carry both.
      error.title = detail;
      error.hidden = false;
      toast(message, 'error');
    };
    // `event.currentTarget` is null as soon as the handler yields, so every
    // button is captured BEFORE the first await; re-enabling is skipped for a
    // button a repaint already replaced.
    const reenable = (button) => {
      if (button.isConnected) button.removeAttribute('disabled');
    };
    pane.querySelector('[data-act="rename"]').addEventListener('click', async (event) => {
      const button = event.currentTarget;
      const name = String(pane.querySelector('[data-field="name"]').value || '').trim();
      error.hidden = true;
      if (!name) {
        fail(new Error(T('err_name_required')));
        return;
      }
      button.setAttribute('disabled', '');
      try {
        await AgentAccounts.update({ accountId, displayName: name });
        toast(T('renamed'), 'success');
        notifyChanged();
        await reload();
      } catch (err) {
        fail(err);
      } finally {
        reenable(button);
      }
    });
    pane.querySelector('[data-field="enabled"]')?.addEventListener('change', async (event) => {
      const toggle = event.currentTarget;
      const wantEnabled = toggle.hasAttribute('checked');
      error.hidden = true;
      // Re-enabling restores the TRUE state, not "active": an account whose
      // credential was never stored is waiting for one, and saying "signed in"
      // because somebody flipped a switch would be a lie the operator acts on.
      const revision = Number(state.account.credential_revision ?? 0);
      const restored = revision > 0
        ? 'active'
        : (state.account.credential_kind === 'api_key' ? 'pending' : 'needs_login');
      try {
        await AgentAccounts.update({ accountId, status: wantEnabled ? restored : 'disabled' });
        notifyChanged();
        await reload();
      } catch (err) {
        fail(err);
        toggle.toggleAttribute('checked', !wantEnabled);
      }
    });
    pane.querySelector('[data-act="key-save"]')?.addEventListener('click', async (event) => {
      const button = event.currentTarget;
      const input = pane.querySelector('[data-field="key"]');
      const material = String(input.value || '').trim();
      error.hidden = true;
      if (!material) {
        fail(new Error(T('err_key_required')));
        return;
      }
      button.setAttribute('disabled', '');
      try {
        await AgentAccounts.setCredential(accountId, material);
        input.value = '';
        toast(T('key_saved'), 'success');
        notifyChanged();
        await reload();
      } catch (err) {
        fail(err);
      } finally {
        reenable(button);
      }
    });
    pane.querySelector('[data-act="key-clear"]')?.addEventListener('click', async (event) => {
      const button = event.currentTarget;
      error.hidden = true;
      const ok = await TfWindow.confirm({
        title: T('key_clear_confirm_title'),
        message: T('key_clear_confirm_body'),
        confirmLabel: T('action_key_clear'),
        cancelLabel: I18n.t('common.cancel'),
        danger: true,
      });
      if (!ok) return;
      button.setAttribute('disabled', '');
      try {
        await AgentAccounts.clearCredential(accountId);
        toast(T('key_cleared'), 'success');
        notifyChanged();
        await reload();
      } catch (err) {
        fail(err);
      } finally {
        reenable(button);
      }
    });
  }

  // ---- Dostęp (A04) ------------------------------------------------------
  async function loadDirectory() {
    if (state.directoryLoaded) return;
    const [users, groups] = await Promise.all([
      ApiBinary.action('iamListUsersRequest').then((r) => r?.users ?? []).catch(() => []),
      ApiBinary.action('iamListGroupsRequest').then((r) => r?.groups ?? []).catch(() => []),
    ]);
    state.users = Array.isArray(users) ? users : [];
    state.groups = Array.isArray(groups) ? groups : [];
    state.directoryLoaded = true;
  }

  function subjectKey(grant) {
    return `${grant.subject_type ?? grant.subjectType}:${grant.subject_id ?? grant.subjectId ?? ''}`;
  }

  function paintAccess() {
    const sessionsByUser = new Map();
    for (const session of state.sessions) {
      const id = session.user_id ?? session.userId ?? '';
      sessionsByUser.set(id, (sessionsByUser.get(id) ?? 0) + 1);
    }
    pane.innerHTML = `
      <div class="tf-toolbar">
        <tf-segmented data-field="mode" size="md" value="${state.orgWide ? 'org' : 'selected'}">
          <option value="selected">${escapeHtml(T('access_mode_selected'))}</option>
          <option value="org">${escapeHtml(T('access_mode_org'))}</option>
        </tf-segmented>
        <span class="tf-toolbar-spacer"></span>
        <tf-combobox data-field="subject" clearable
          placeholder="${escapeAttr(T('access_add_placeholder'))}"></tf-combobox>
        <tf-button variant="primary" data-act="save-access" disabled>${escapeHtml(T('access_save'))}</tf-button>
      </div>
      <tf-table variant="flush" data-table="grants" empty-message="${escapeAttr(T('access_empty'))}">
        <tf-column key="who" label="${escapeAttr(T('col_who'))}" renderer="html" fill></tf-column>
        <tf-column key="kind" label="${escapeAttr(T('col_kind'))}"></tf-column>
        <tf-column key="granted" label="${escapeAttr(T('col_granted_by'))}"></tf-column>
        <tf-column key="sessions" label="${escapeAttr(T('col_active_sessions'))}"></tf-column>
      </tf-table>
      <div class="aa-foot" data-access-foot></div>
      <p class="aa-note">${escapeHtml(T('access_note'))}</p>
      <p class="aa-error" role="alert" data-error hidden></p>`;

    const mode = pane.querySelector('[data-field="mode"]');
    const picker = pane.querySelector('[data-field="subject"]');
    const save = pane.querySelector('[data-act="save-access"]');
    const table = pane.querySelector('[data-table="grants"]');
    const error = pane.querySelector('[data-error]');

    const userName = (user) => String(user.displayName ?? user.display_name ?? user.username ?? user.email ?? user.id ?? '');
    const userSub = (id) => {
      const user = state.users.find((u) => String(u.id) === String(id));
      return user?.email ? String(user.email) : '';
    };

    const paintPicker = () => {
      const taken = new Set(state.subjects.map(subjectKey));
      const options = [
        ...state.users
          .filter((user) => !taken.has(`user:${user.id}`))
          .map((user) => ({
            value: `user:${user.id}`,
            label: userName(user),
            description: user.email ? String(user.email) : T('col_user'),
            group: T('access_group_users'),
          })),
        ...state.groups
          .filter((group) => !taken.has(`group:${group.id}`))
          .map((group) => ({
            value: `group:${group.id}`,
            label: String(group.name ?? group.id),
            description: T('access_members', { count: Number(group.memberCount ?? group.member_count ?? 0) }),
            group: T('access_group_groups'),
          })),
      ];
      picker.options = options;
      picker.toggleAttribute('disabled', state.orgWide || !options.length);
    };

    const paintRows = () => {
      const rows = state.orgWide
        ? [{
            who: `<div class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(T('access_whole_org'))}</div>`
              + `<div class="tf-table__cell-sub">${escapeHtml(T('access_whole_org_sub'))}</div>`,
            kind: T('subject_org'),
            granted: grantedByLabel(state.grants.find((g) => (g.subject_type ?? g.subjectType) === 'org') ?? {}),
            sessions: '—',
            _subject: null,
          }]
        : state.subjects.map((grant) => {
          const type = grant.subject_type ?? grant.subjectType;
          const id = grant.subject_id ?? grant.subjectId ?? '';
          const members = grant.member_count ?? grant.memberCount;
          const known = type === 'user'
            ? state.users.find((u) => String(u.id) === String(id))
            : state.groups.find((g) => String(g.id) === String(id));
          const title = grant.display_name
            || (type === 'user' ? (known ? userName(known) : id) : String(known?.name ?? id));
          const sub = type === 'user'
            ? userSub(id)
            : T('access_members', { count: Number(members ?? known?.memberCount ?? known?.member_count ?? 0) });
          return {
            who: `<div class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(title)}</div>`
              + (sub ? `<div class="tf-table__cell-sub">${escapeHtml(sub)}</div>` : ''),
            kind: T(type === 'group' ? 'subject_group' : 'subject_user'),
            // Who granted it, from the STORED grant: a subject the operator has
            // just added is not granted by anybody yet, and printing their own
            // name next to it would claim a write that has not happened.
            granted: grantedByLabel(state.grants.find((g) => subjectKey(g) === `${type}:${id}`) ?? {}),
            // A group's people are not enumerated here, so its session count is
            // not known from this response — an em dash, never a zero.
            sessions: type === 'user' ? String(sessionsByUser.get(id) ?? 0) : '—',
            _subject: `${type}:${id}`,
          };
        });
      table.rowActions = state.orgWide ? null : (row, _idx, currentRow) => {
        const button = document.createElement('tf-button');
        button.setAttribute('variant', 'ghost');
        button.setAttribute('size', 'sm');
        button.textContent = T('action_revoke');
        button.addEventListener('click', () => {
          const live = currentRow?.() ?? row;
          state.subjects = state.subjects.filter((grant) => subjectKey(grant) !== live._subject);
          state.accessDirty = true;
          paintPicker();
          paintRows();
          paintSave();
        });
        return button;
      };
      table.rows = rows;
      const foot = pane.querySelector('[data-access-foot]');
      foot.textContent = state.orgWide
        ? T('access_foot_org')
        : T('access_foot', { count: state.subjects.length });
    };

    const paintSave = () => save.toggleAttribute('disabled', !state.accessDirty);

    mode.addEventListener('change', () => {
      state.orgWide = mode.value === 'org';
      state.accessDirty = true;
      paintPicker();
      paintRows();
      paintSave();
    });
    picker.addEventListener('change', (event) => {
      const value = event.detail?.value;
      if (!value) return;
      const [type, id] = String(value).split(':');
      if (!type || !id) return;
      state.subjects = [...state.subjects, { subject_type: type, subject_id: id, display_name: event.detail?.label ?? '' }];
      state.accessDirty = true;
      picker.setAttribute('value', '');
      paintPicker();
      paintRows();
      paintSave();
    });
    save.addEventListener('click', async () => {
      error.hidden = true;
      save.setAttribute('disabled', '');
      const grants = state.orgWide
        ? [{ subject_type: 'org', subject_id: '' }]
        : state.subjects.map((grant) => ({
          subject_type: grant.subject_type ?? grant.subjectType,
          subject_id: grant.subject_id ?? grant.subjectId,
        }));
      try {
        const response = await AgentAccounts.setGrants(accountId, grants);
        state.grants = response?.grants ?? grants;
        state.accessDirty = false;
        toast(T('access_saved'), 'success');
        notifyChanged();
        await reload();
      } catch (err) {
        const { message, detail } = describeError(err);
        error.textContent = message;
        error.title = detail;
        error.hidden = false;
        toast(message, 'error');
      } finally {
        paintSave();
      }
    });

    paintPicker();
    paintRows();
    paintSave();
    loadDirectory().then(() => {
      if (!host.isConnected || state.tab !== 'access') return;
      paintPicker();
      paintRows();
    });
  }

  // ---- Agenci ------------------------------------------------------------
  function paintAgents() {
    pane.innerHTML = `
      <tf-table variant="flush" data-table="agents" empty-message="${escapeAttr(T('agents_empty'))}">
        <tf-column key="agent" label="${escapeAttr(T('col_agent'))}" renderer="html" fill></tf-column>
        <tf-column key="mode" label="${escapeAttr(T('col_bind_mode'))}"></tf-column>
      </tf-table>
      <div class="aa-foot">${escapeHtml(T('agents_foot', { count: state.agents.length }))}</div>
      <p class="aa-note">${escapeHtml(T('agents_note'))}</p>`;
    pane.querySelector('[data-table="agents"]').rows = state.agents.map((agent) => ({
      agent: `<div class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(agent.agent_name ?? '')}</div>`
        + `<div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(shortId(agent.agent_id))}</div>`,
      mode: T(agent.bind_mode === 'user' ? 'bind_mode_user' : 'bind_mode_global'),
    }));
  }

  body.querySelector('[data-tabs]').addEventListener('change', (event) => {
    state.tab = event.detail?.value || 'overview';
    paint();
  });

  footer.querySelector('[data-act="delete"]').addEventListener('click', async () => {
    const confirmed = await TfWindow.confirm({
      title: T('delete_confirm_title'),
      message: T('delete_confirm_body', { name: escapeHtml(state.account.display_name ?? '') }),
      description: T('delete_confirm_hint'),
      confirmLabel: T('action_delete'),
      cancelLabel: I18n.t('common.cancel'),
      danger: true,
    });
    if (!confirmed) return;
    try {
      await AgentAccounts.remove(accountId);
      toast(T('deleted'), 'success');
      host.close();
      onChanged?.();
    } catch (err) {
      toast(errorText(err), 'error');
    }
  });

  paint();
}
