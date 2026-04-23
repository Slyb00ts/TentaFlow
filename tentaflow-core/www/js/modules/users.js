// =============================================================================
// Plik: modules/users.js
// Opis: Admin screen: lista userow + grupy. Uzywa <tf-screen> jako shell.
//       Naglowek = slot="breadcrumb" + slot="header" (.tf-page-header albo
//       .tf-detail-header) + slot="tabs" (<tf-tabs>). Body na widokach detail
//       to stack .tf-section-card (brak split/aside — wzorzec wg mockupu
//       addons-permissions). Nawigacja wstecz idzie przez breadcrumb.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let me = null;
let users = [];
let groups = [];
let activeTab = 'users'; // 'users' | 'groups'
let filter = 'all';
let searchQuery = '';
// View state: 'list' | 'user-detail' | 'group-detail'.
let view = 'list';
let detailEntityId = null;
let detailTab = 'profile';

const UsersScreen = {
  title: 'Użytkownicy',
  render() {
    return `<div id="users-root"></div>`;
  },
  async mount() {
    try {
      me = await ApiBinary.one('authMeRequest');
    } catch { me = null; }
    const root = byId('users-root');
    if (!root) return;
    if (!me || (me.role !== 'admin' && !me.isAdmin)) {
      root.innerHTML = `<div class="card"><p>${escapeHtml(I18n.t('users.admin_only'))}</p></div>`;
      return;
    }
    await loadData();
    renderActive();
  },
  unmount() {
    me = null; users = []; groups = [];
  },
};

async function loadData() {
  try {
    const [u, g] = await Promise.all([
      ApiBinary.action('iamListUsersRequest').then((r) => r?.users ?? []),
      ApiBinary.action('iamListGroupsRequest').then((r) => r?.groups ?? []),
    ]);
    users = Array.isArray(u) ? u : [];
    groups = Array.isArray(g) ? g : [];
  } catch (e) {
    toast(e.message || I18n.t('users.load_failed'), 'error');
  }
}

function renderActive() {
  const root = byId('users-root');
  if (!root) return;
  if (view === 'user-detail') {
    const u = users.find((x) => x.id === detailEntityId);
    if (!u) { view = 'list'; return renderActive(); }
    renderUserDetailScreen(root, u);
    return;
  }
  if (view === 'group-detail') {
    const g = groups.find((x) => x.id === detailEntityId);
    if (!g) { view = 'list'; return renderActive(); }
    renderGroupDetailScreen(root, g);
    return;
  }
  renderListScreen(root);
}

// ---- LIST SCREEN ----

function renderListScreen(root) {
  const isUsers = activeTab === 'users';
  const active = users.filter((u) => u.isActive).length;
  const inactive = users.length - active;
  const admin = users.filter((u) => u.role === 'admin').length;
  const subtitle = isUsers
    ? `${I18n.t('users.count_users', { n: users.length })} · ${I18n.t('users.sub_active', { n: active })} · ${I18n.t('users.sub_inactive', { n: inactive })} · ${I18n.t('users.sub_admin', { n: admin })}`
    : I18n.t('users.count_groups', { n: groups.length });
  const title = isUsers ? I18n.t('users.title') : I18n.t('users.tab_groups');
  const count = isUsers ? users.length : groups.length;
  const addLabel = isUsers ? I18n.t('users.new_user') : I18n.t('users.new_group');
  const addId = isUsers ? 'btn-add-user' : 'btn-add-group';

  const searchToolbar = isUsers ? `
    <div class="users-toolbar">
      <tf-searchbox id="users-search" placeholder="${escapeAttr(I18n.t('users.search_ph'))}" debounce="120" value="${escapeAttr(searchQuery)}"></tf-searchbox>
      <div class="tf-filter-group" id="users-filter-group">
        <tf-chip class="filter-chip" clickable ${filter === 'all' ? 'active' : ''} data-filter="all">${escapeHtml(I18n.t('users.filter_all'))}</tf-chip>
        <tf-chip class="filter-chip" clickable ${filter === 'active' ? 'active' : ''} data-filter="active">${escapeHtml(I18n.t('users.filter_active'))}</tf-chip>
        <tf-chip class="filter-chip" clickable ${filter === 'admin' ? 'active' : ''} data-filter="admin">${escapeHtml(I18n.t('users.filter_admin'))}</tf-chip>
        <tf-chip class="filter-chip" clickable ${filter === 'inactive' ? 'active' : ''} data-filter="inactive">${escapeHtml(I18n.t('users.filter_inactive'))}</tf-chip>
        <tf-chip class="filter-chip" clickable ${filter === 'sso' ? 'active' : ''} data-filter="sso">${escapeHtml(I18n.t('users.filter_sso'))}</tf-chip>
      </div>
    </div>
  ` : '';

  root.innerHTML = `
    <tf-screen>
      <div slot="breadcrumb" class="tf-breadcrumb">
        <span class="crumb current">${escapeHtml(title)}</span>
      </div>
      <div slot="header" class="tf-page-header">
        <div class="titles">
          <h1>${escapeHtml(title)} <span style="color:var(--text-3);font-weight:600">· ${count}</span></h1>
          <div class="sub">${escapeHtml(subtitle)}</div>
        </div>
        <div class="actions">
          <tf-button variant="primary" icon="plus" id="${addId}">${escapeHtml(addLabel)}</tf-button>
        </div>
      </div>
      <tf-tabs slot="tabs" variant="underline" value="${activeTab}" id="users-tabs-nav">
        <tf-tab id="users" icon="user" count="${users.length}">${escapeHtml(I18n.t('users.tab_users'))}</tf-tab>
        <tf-tab id="groups" icon="users" count="${groups.length}">${escapeHtml(I18n.t('users.tab_groups'))}</tf-tab>
      </tf-tabs>
      ${searchToolbar}
      <div class="tf-section-card" style="padding:0;" id="users-list-body">${isUsers ? renderUsersTable() : renderGroupsTable()}</div>
    </tf-screen>
  `;

  const screen = root.querySelector('tf-screen');
  byId('users-tabs-nav')?.addEventListener('change', (e) => {
    activeTab = e.detail?.value || 'users';
    renderActive();
  });
  byId(addId)?.addEventListener('click', isUsers ? openCreateUserModal : openCreateGroupModal);
  screen?.addEventListener('click', handleListClick);
  const sb = byId('users-search');
  if (sb) {
    sb.addEventListener('input', (e) => {
      searchQuery = e.detail?.value ?? e.target?.value ?? '';
      rerenderUsersTable();
    });
  }
}

function filteredUsers() {
  const q = searchQuery.trim().toLowerCase();
  return users.filter((u) => {
    if (filter === 'active' && !u.isActive) return false;
    if (filter === 'inactive' && u.isActive) return false;
    if (filter === 'admin' && u.role !== 'admin') return false;
    if (filter === 'sso' && !u.ssoProvider) return false;
    if (!q) return true;
    const hay = `${u.username} ${u.displayName || ''} ${u.email || ''}`.toLowerCase();
    return hay.includes(q);
  });
}

function renderUsersTable() {
  const list = filteredUsers();
  if (list.length === 0) {
    const empty = users.length === 0 ? I18n.t('users.no_users') : I18n.t('users.no_match');
    return `<div class="users-empty">${escapeHtml(empty)}</div>`;
  }
  const body = list.map(renderUserRow).join('');
  return `
    <table class="tf-accounts-table">
      <thead>
        <tr>
          <th>${escapeHtml(I18n.t('users.col_user'))}</th>
          <th>${escapeHtml(I18n.t('users.col_role'))}</th>
          <th>${escapeHtml(I18n.t('users.col_groups'))}</th>
          <th>${escapeHtml(I18n.t('users.col_source'))}</th>
          <th>${escapeHtml(I18n.t('users.col_last_login'))}</th>
          <th class="actions-col">${escapeHtml(I18n.t('users.col_actions'))}</th>
        </tr>
      </thead>
      <tbody>${body}</tbody>
    </table>
  `;
}

function rerenderUsersTable() {
  const host = byId('users-list-body');
  if (!host) return;
  host.innerHTML = renderUsersTable();
  // Reset active state chip jesli nie ma selektora .filter-chip[active]
  const fg = byId('users-filter-group');
  if (fg) {
    fg.querySelectorAll('.filter-chip').forEach((c) => {
      if (c.dataset.filter === filter) c.setAttribute('active', '');
      else c.removeAttribute('active');
    });
  }
}

function initials(u) {
  return (u.displayName || u.username || '?')
    .split(/\s+/).map((p) => p[0]).filter(Boolean).slice(0, 2).join('').toUpperCase();
}

function formatRelative(epochSeconds) {
  if (!epochSeconds) return '—';
  const diff = Math.floor(Date.now() / 1000) - Number(epochSeconds);
  if (diff < 60) return I18n.t('users.time_now');
  if (diff < 3600) return I18n.t('users.time_min', { n: Math.floor(diff / 60) });
  if (diff < 86400) return I18n.t('users.time_hour', { n: Math.floor(diff / 3600) });
  if (diff < 86400 * 7) return I18n.t('users.time_day', { n: Math.floor(diff / 86400) });
  if (diff < 86400 * 60) return I18n.t('users.time_week', { n: Math.floor(diff / 86400 / 7) });
  return I18n.t('users.time_month', { n: Math.floor(diff / 86400 / 30) });
}

function renderUserRow(u) {
  const roleLabel = { admin: I18n.t('users.role_admin'), power_user: I18n.t('users.role_power'), user: I18n.t('users.role_user') }[u.role] || 'User';
  const roleClass = u.role === 'admin' ? 'role-admin' : u.role === 'power_user' ? 'role-power' : 'role-user';
  const statusPill = u.isActive
    ? `<span class="status-pill ok">${escapeHtml(I18n.t('users.status_active'))}</span>`
    : `<span class="status-pill off">${escapeHtml(I18n.t('users.status_inactive'))}</span>`;
  const sourcePill = u.ssoProvider
    ? `<span class="status-pill sso">SSO · ${escapeHtml(u.ssoProvider)}</span>`
    : `<span class="status-pill local">Local</span>`;
  const groupTagsHtml = (u.groupIds || [])
    .map((gid) => groups.find((g) => g.id === gid))
    .filter(Boolean)
    .map((g) => `<span class="group-tag">${escapeHtml(g.name)}</span>`)
    .join('');
  return `
    <tr data-user-id="${u.id}">
      <td>
        <div class="tf-account-cell">
          <div class="tf-account-avatar">${escapeHtml(initials(u))}</div>
          <div>
            <div class="tf-account-name">${escapeHtml(u.displayName || u.username)} ${statusPill}</div>
            <div class="tf-account-sub">${escapeHtml(u.email || u.username)}</div>
          </div>
        </div>
      </td>
      <td><span class="role-pill ${roleClass}">${escapeHtml(roleLabel)}</span></td>
      <td><div class="group-tag-list">${groupTagsHtml || `<span style="color:var(--text-3); font-size:11px;">—</span>`}</div></td>
      <td>${sourcePill}</td>
      <td style="color:var(--text-3); font-size:11px;">${escapeHtml(formatRelative(u.lastLoginAt))}</td>
      <td class="actions-col">
        <div class="row-actions">
          <tf-button variant="ghost" size="sm" icon="edit" data-action="edit-user" title="${escapeAttr(I18n.t('users.edit'))}"></tf-button>
          <tf-button variant="ghost" size="sm" icon="trash" data-action="delete-user" title="${escapeAttr(I18n.t('users.delete'))}"></tf-button>
        </div>
      </td>
    </tr>
  `;
}

function renderGroupsTable() {
  if (groups.length === 0) {
    return `<div class="users-empty">${escapeHtml(I18n.t('users.no_groups_yet'))}</div>`;
  }
  const body = groups.map(renderGroupRow).join('');
  return `
    <table class="tf-accounts-table">
      <thead>
        <tr>
          <th>${escapeHtml(I18n.t('users.col_group'))}</th>
          <th>${escapeHtml(I18n.t('users.col_members'))}</th>
          <th>${escapeHtml(I18n.t('users.col_description'))}</th>
          <th class="actions-col">${escapeHtml(I18n.t('users.col_actions'))}</th>
        </tr>
      </thead>
      <tbody>${body}</tbody>
    </table>
  `;
}

function renderGroupRow(g) {
  const gIcon = `<svg viewBox="0 0 24 24"><use href="#i-users"/></svg>`;
  return `
    <tr data-group-id="${g.id}">
      <td>
        <div class="tf-account-cell">
          <div class="tf-account-avatar">${gIcon}</div>
          <div>
            <div class="tf-account-name">${escapeHtml(g.name)}</div>
            <div class="tf-account-sub">group_${escapeHtml(String(g.id))}</div>
          </div>
        </div>
      </td>
      <td><span class="group-tag">${escapeHtml(I18n.t('users.members_count', { n: g.memberCount || 0 }))}</span></td>
      <td style="color:var(--text-2); font-size:12px;">${escapeHtml(g.description || '—')}</td>
      <td class="actions-col">
        <div class="row-actions">
          <tf-button variant="ghost" size="sm" icon="edit" data-action="edit-group" title="${escapeAttr(I18n.t('users.edit'))}"></tf-button>
          <tf-button variant="ghost" size="sm" icon="trash" data-action="delete-group" title="${escapeAttr(I18n.t('users.delete'))}"></tf-button>
        </div>
      </td>
    </tr>
  `;
}

function handleListClick(e) {
  const row = e.target.closest('[data-user-id]');
  const groupRow = e.target.closest('[data-group-id]');
  const actionBtn = e.target.closest('[data-action]');
  if (actionBtn) {
    e.stopPropagation();
    const action = actionBtn.dataset.action;
    if (action === 'edit-user' && row) {
      openUserDetail(Number(row.dataset.userId));
    } else if (action === 'delete-user' && row) {
      const u = users.find((x) => String(x.id) === row.dataset.userId);
      if (u) confirmDeleteUser(u);
    } else if (action === 'edit-group' && groupRow) {
      openGroupDetail(Number(groupRow.dataset.groupId));
    } else if (action === 'delete-group' && groupRow) {
      const g = groups.find((x) => String(x.id) === groupRow.dataset.groupId);
      if (g) confirmDeleteGroup(g);
    }
    return;
  }
  const chipFilter = e.target.closest('[data-filter]');
  if (chipFilter) {
    filter = chipFilter.dataset.filter;
    rerenderUsersTable();
    return;
  }
  if (row) {
    openUserDetail(Number(row.dataset.userId));
  } else if (groupRow) {
    openGroupDetail(Number(groupRow.dataset.groupId));
  }
}

function openUserDetail(id) {
  detailEntityId = id;
  detailTab = 'profile';
  view = 'user-detail';
  renderActive();
}

function openGroupDetail(id) {
  detailEntityId = id;
  detailTab = 'info';
  view = 'group-detail';
  renderActive();
}

function backToList() {
  view = 'list';
  detailEntityId = null;
  renderActive();
}

// ---- USER DETAIL SCREEN ----

function renderUserDetailScreen(root, u) {
  const userGroups = (u.groupIds || [])
    .map((gid) => groups.find((g) => g.id === gid))
    .filter(Boolean);
  const displayName = u.displayName || u.username;
  const subLine = `${u.email || u.username} · user_${u.id}`;
  const chev = '<svg viewBox="0 0 24 24"><polyline points="9 18 15 12 9 6"/></svg>';

  // Badges w naglowku: status + zrodlo (Local/SSO) + liczba grup.
  const statusChip = u.isActive
    ? `<tf-chip variant="success">${escapeHtml(I18n.t('users.status_active'))}</tf-chip>`
    : `<tf-chip variant="muted">${escapeHtml(I18n.t('users.status_inactive'))}</tf-chip>`;
  const sourceChip = u.ssoProvider
    ? `<tf-chip variant="info">SSO · ${escapeHtml(u.ssoProvider)}</tf-chip>`
    : `<tf-chip variant="neutral">Local</tf-chip>`;
  const groupsChip = `<tf-chip variant="accent">${escapeHtml(I18n.t('users.count_groups', { n: userGroups.length }))}</tf-chip>`;

  root.innerHTML = `
    <tf-screen>
      <div slot="breadcrumb" class="tf-breadcrumb">
        <span class="crumb" data-action="back">${escapeHtml(I18n.t('users.title'))}</span>
        <span class="sep">${chev}</span>
        <span class="crumb current">${escapeHtml(displayName)}</span>
      </div>
      <div slot="header" class="tf-detail-header">
        <div class="big-ico">${escapeHtml(initials(u))}</div>
        <div class="d-meta">
          <div class="d-name">${escapeHtml(displayName)}</div>
          <div class="d-sub">${escapeHtml(subLine)}</div>
          <div class="d-badges">${statusChip}${sourceChip}${groupsChip}</div>
        </div>
        <div class="d-actions">
          <tf-button variant="ghost" icon="key" data-action="reset-pw">${escapeHtml(I18n.t('users.reset_password'))}</tf-button>
          <tf-button variant="danger" data-action="delete-user">${escapeHtml(I18n.t('users.delete_account'))}</tf-button>
        </div>
      </div>
      <tf-tabs slot="tabs" variant="underline" value="${detailTab}" id="ud-tabs">
        <tf-tab id="profile" icon="user">${escapeHtml(I18n.t('users.tab_profile'))}</tf-tab>
        <tf-tab id="groups" icon="users" count="${userGroups.length}">${escapeHtml(I18n.t('users.tab_memberships'))}</tf-tab>
        <tf-tab id="perms" icon="shield">${escapeHtml(I18n.t('users.tab_perms'))}</tf-tab>
      </tf-tabs>
      <div id="ud-main">${renderUserDetailPanel(u)}</div>
    </tf-screen>
  `;
  wireUserDetail(root, u);
}

function renderUserDetailPanel(u) {
  const selectedGroupIds = new Set(u.groupIds || []);
  if (detailTab === 'profile') {
    return `
      <div class="tf-section-card">
        <h3>${escapeHtml(I18n.t('users.tab_profile'))}</h3>
        <div class="users-form-row">
          <div class="field"><tf-input id="u-username" label="${escapeAttr(I18n.t('users.field_username'))}" value="${escapeAttr(u.username || '')}" disabled></tf-input></div>
          <div class="field"><tf-input id="u-display" label="${escapeAttr(I18n.t('users.field_display'))}" value="${escapeAttr(u.displayName || '')}"></tf-input></div>
        </div>
        <div class="users-form-row full">
          <div class="field"><tf-input id="u-email" type="email" label="${escapeAttr(I18n.t('users.field_email'))}" value="${escapeAttr(u.email || '')}"></tf-input></div>
        </div>
      </div>
      <div class="tf-section-card">
        <h3>${escapeHtml(I18n.t('users.field_role'))} &middot; ${escapeHtml(I18n.t('users.field_status'))}</h3>
        <div class="users-form-row">
          <div class="field">
            <tf-select id="u-role" label="${escapeAttr(I18n.t('users.field_role'))}" value="${escapeAttr(u.role || 'user')}">
              <option value="user">${escapeHtml(I18n.t('users.role_user_desc'))}</option>
              <option value="power_user">${escapeHtml(I18n.t('users.role_power_desc'))}</option>
              <option value="admin">${escapeHtml(I18n.t('users.role_admin_desc'))}</option>
            </tf-select>
          </div>
          <div class="field">
            <tf-select id="u-status" label="${escapeAttr(I18n.t('users.field_status'))}" value="${u.isActive ? 'active' : 'disabled'}">
              <option value="active">${escapeHtml(I18n.t('users.status_active_desc'))}</option>
              <option value="disabled">${escapeHtml(I18n.t('users.status_disabled_desc'))}</option>
            </tf-select>
          </div>
        </div>
        <div class="section-footer">
          <tf-button variant="ghost" data-action="back">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
          <tf-button variant="primary" data-action="save-profile">${escapeHtml(I18n.t('users.save_changes'))}</tf-button>
        </div>
        <div class="form-error" hidden></div>
      </div>
    `;
  }
  if (detailTab === 'groups') {
    return `
      <div class="tf-section-card">
        <h3>${escapeHtml(I18n.t('users.field_groups'))}</h3>
        <div id="u-groups-picker">${renderGroupPicker(selectedGroupIds)}</div>
        <div class="section-footer">
          <tf-button variant="primary" data-action="save-groups">${escapeHtml(I18n.t('users.save_changes'))}</tf-button>
        </div>
      </div>
    `;
  }
  return `
    <div class="tf-section-card">
      <h3>${escapeHtml(I18n.t('users.tab_perms'))}</h3>
      <div class="users-empty">${escapeHtml(I18n.t('users.perms_user_todo'))}</div>
    </div>
  `;
}

function wireUserDetail(host, u) {
  const selectedGroupIds = new Set(u.groupIds || []);

  const tabsEl = host.querySelector('#ud-tabs');
  tabsEl?.addEventListener('change', (e) => {
    detailTab = e.detail?.value || 'profile';
    const main = host.querySelector('#ud-main');
    if (main) main.innerHTML = renderUserDetailPanel(u);
    const pickerHost = host.querySelector('#u-groups-picker');
    if (pickerHost) wireGroupPicker(pickerHost, selectedGroupIds);
  });

  host.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    const action = btn.dataset.action;
    if (action === 'back') return backToList();
    if (action === 'delete-user') {
      if (!confirm(I18n.t('users.confirm_delete_user', { name: u.username }))) return;
      try {
        await ApiBinary.action('iamDeleteUserRequest', { userId: u.id });
        toast(I18n.t('users.deleted'), 'success');
        await loadData();
        backToList();
      } catch (err) { toast(err.message, 'error'); }
      return;
    }
    if (action === 'reset-pw') {
      const newPw = prompt(I18n.t('users.reset_prompt'));
      if (!newPw) return;
      try {
        await ApiBinary.action('iamResetUserPasswordRequest', { userId: u.id, newPassword: newPw });
        toast(I18n.t('users.password_reset'), 'success');
      } catch (err) { toast(err.message, 'error'); }
      return;
    }
    if (action === 'save-profile') {
      try {
        await ApiBinary.action('iamUpdateUserRequest', {
          userId: u.id,
          displayName: host.querySelector('#u-display').value.trim(),
          email: host.querySelector('#u-email').value.trim(),
          isActive: host.querySelector('#u-status').value === 'active',
          role: host.querySelector('#u-role').value,
        });
        toast(I18n.t('users.saved'), 'success');
        await loadData();
        renderActive();
      } catch (err) {
        const fe = host.querySelector('.form-error');
        if (fe) { fe.hidden = false; fe.textContent = err.message; }
      }
      return;
    }
    if (action === 'save-groups') {
      try {
        await ApiBinary.action('iamSetUserGroupsRequest', { userId: u.id, groupIds: Array.from(selectedGroupIds) });
        toast(I18n.t('users.saved'), 'success');
        await loadData();
        renderActive();
      } catch (err) { toast(err.message, 'error'); }
    }
  });

  const pickerHost = host.querySelector('#u-groups-picker');
  if (pickerHost) wireGroupPicker(pickerHost, selectedGroupIds);
}

// ---- GROUP DETAIL SCREEN ----

function renderGroupDetailScreen(root, g) {
  const membersCount = g.memberCount || 0;
  const subLine = `group_${g.id}${g.description ? ' · ' + g.description : ''}`;
  const chev = '<svg viewBox="0 0 24 24"><polyline points="9 18 15 12 9 6"/></svg>';
  const groupIco = '<svg viewBox="0 0 24 24" stroke="currentColor" fill="none" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 0 0-3-3.87"/></svg>';
  const membersChip = `<tf-chip variant="accent">${escapeHtml(I18n.t('users.members_count', { n: membersCount }))}</tf-chip>`;

  root.innerHTML = `
    <tf-screen>
      <div slot="breadcrumb" class="tf-breadcrumb">
        <span class="crumb" data-action="back">${escapeHtml(I18n.t('users.tab_groups'))}</span>
        <span class="sep">${chev}</span>
        <span class="crumb current">${escapeHtml(g.name)}</span>
      </div>
      <div slot="header" class="tf-detail-header">
        <div class="big-ico" style="background:linear-gradient(135deg,#a78bfa,#6366f1);">${groupIco}</div>
        <div class="d-meta">
          <div class="d-name">${escapeHtml(g.name)}</div>
          <div class="d-sub">${escapeHtml(subLine)}</div>
          <div class="d-badges">${membersChip}</div>
        </div>
        <div class="d-actions">
          <tf-button variant="danger" data-action="delete-group">${escapeHtml(I18n.t('users.delete_group'))}</tf-button>
        </div>
      </div>
      <tf-tabs slot="tabs" variant="underline" value="${detailTab}" id="gd-tabs">
        <tf-tab id="info" icon="info">${escapeHtml(I18n.t('users.tab_info'))}</tf-tab>
        <tf-tab id="members" icon="users" count="${membersCount}">${escapeHtml(I18n.t('users.tab_members'))}</tf-tab>
        <tf-tab id="perms" icon="shield">${escapeHtml(I18n.t('users.tab_perms'))}</tf-tab>
      </tf-tabs>
      <div id="gd-main">${renderGroupDetailPanel(g)}</div>
    </tf-screen>
  `;
  wireGroupDetail(root, g);
}

function renderGroupDetailPanel(g) {
  if (detailTab === 'info') {
    return `
      <div class="tf-section-card">
        <h3>${escapeHtml(I18n.t('users.tab_info'))}</h3>
        <div class="users-form-row">
          <div class="field"><tf-input id="g-name" label="${escapeAttr(I18n.t('users.field_group_name'))}" value="${escapeAttr(g.name || '')}"></tf-input></div>
          <div class="field"><tf-input id="g-id" label="ID" value="${escapeAttr(String(g.id))}" disabled></tf-input></div>
        </div>
        <div class="users-form-row full">
          <div class="field"><tf-input id="g-descr" label="${escapeAttr(I18n.t('users.field_group_desc'))}" value="${escapeAttr(g.description || '')}"></tf-input></div>
        </div>
        <div class="section-footer">
          <tf-button variant="ghost" data-action="back">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
          <tf-button variant="primary" data-action="save-info">${escapeHtml(I18n.t('users.save_changes'))}</tf-button>
        </div>
        <div class="form-error" hidden></div>
      </div>
    `;
  }
  if (detailTab === 'members') {
    return `
      <div class="tf-section-card">
        <h3>${escapeHtml(I18n.t('users.tab_members'))}</h3>
        <div id="gd-members-list"><div class="mesh-loading">${escapeHtml(I18n.t('common.loading'))}</div></div>
      </div>
    `;
  }
  return `
    <div class="tf-section-card">
      <h3>${escapeHtml(I18n.t('users.tab_perms'))}</h3>
      <div id="gd-perms-body"><div class="mesh-loading">${escapeHtml(I18n.t('common.loading'))}</div></div>
    </div>
  `;
}

function wireGroupDetail(host, g) {
  const tabsEl = host.querySelector('#gd-tabs');
  const mount = async () => {
    const main = host.querySelector('#gd-main');
    if (!main) return;
    main.innerHTML = renderGroupDetailPanel(g);
    if (detailTab === 'members') {
      await loadGroupMembers(g.id, main.querySelector('#gd-members-list'));
    } else if (detailTab === 'perms') {
      await loadGroupPermissions(g.id, main.querySelector('#gd-perms-body'));
    }
  };
  tabsEl?.addEventListener('change', async (e) => {
    detailTab = e.detail?.value || 'info';
    await mount();
  });

  host.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    const action = btn.dataset.action;
    if (action === 'back') return backToList();
    if (action === 'delete-group') {
      if (!confirm(I18n.t('users.confirm_delete_group', { name: g.name }))) return;
      try {
        await ApiBinary.action('iamDeleteGroupRequest', { groupId: g.id });
        toast(I18n.t('users.group_deleted'), 'success');
        await loadData();
        backToList();
      } catch (err) { toast(err.message, 'error'); }
      return;
    }
    if (action === 'save-info') {
      try {
        await ApiBinary.action('iamUpdateGroupRequest', {
          groupId: g.id,
          name: host.querySelector('#g-name').value.trim(),
          description: host.querySelector('#g-descr').value.trim(),
        });
        toast(I18n.t('users.group_saved'), 'success');
        await loadData();
        renderActive();
      } catch (err) {
        const fe = host.querySelector('.form-error');
        if (fe) { fe.hidden = false; fe.textContent = err.message; }
      }
    }
  });

  // Pre-fetch jesli wchodzimy od razu na taby members/perms.
  if (detailTab === 'members' || detailTab === 'perms') mount();
}

// ---- Modals (create user / create group) ----

function openCreateUserModal() {
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('users.modal_create_user'));
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '540');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const body = document.createElement('div');
  body.slot = 'body';
  const selectedGroupIds = new Set();
  body.innerHTML = `
    <div class="users-form-row">
      <div class="field"><tf-input id="u-username" label="${escapeAttr(I18n.t('users.field_username'))}" placeholder="np. adam.kowalski" required></tf-input></div>
      <div class="field"><tf-input id="u-display" label="${escapeAttr(I18n.t('users.field_display'))}" placeholder="Adam Kowalski"></tf-input></div>
    </div>
    <div class="users-form-row full">
      <div class="field"><tf-input id="u-email" type="email" label="${escapeAttr(I18n.t('users.field_email'))}" placeholder="adam@firma.com"></tf-input></div>
    </div>
    <div class="users-form-row">
      <div class="field"><tf-input id="u-password" type="password" label="${escapeAttr(I18n.t('users.field_password'))}" required></tf-input></div>
      <div class="field">
        <tf-select id="u-role" label="${escapeAttr(I18n.t('users.field_role'))}" value="user">
          <option value="user">${escapeHtml(I18n.t('users.role_user'))}</option>
          <option value="power_user">${escapeHtml(I18n.t('users.role_power'))}</option>
          <option value="admin">${escapeHtml(I18n.t('users.role_admin'))}</option>
        </tf-select>
      </div>
    </div>
    <div class="users-form-row full">
      <div class="field">
        <label class="field-label">${escapeHtml(I18n.t('users.field_groups'))}</label>
        <div id="u-groups-picker">${renderGroupPicker(selectedGroupIds)}</div>
      </div>
    </div>
    <div class="form-hint">${escapeHtml(I18n.t('users.create_hint'))}</div>
    <div class="form-error" hidden></div>
  `;
  win.appendChild(body);
  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.innerHTML = `<tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button><tf-button variant="primary" data-action="save">${escapeHtml(I18n.t('common.create'))}</tf-button>`;
  win.appendChild(foot);
  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);
  const cleanup = () => { win.remove(); backdrop.remove(); };
  wireGroupPicker(body.querySelector('#u-groups-picker'), selectedGroupIds);
  win.addEventListener('action', async (e) => {
    const a = e.detail?.action;
    if (a === 'cancel') return cleanup();
    if (a === 'save') {
      e.preventDefault();
      const payload = {
        username: body.querySelector('#u-username').value.trim(),
        displayName: body.querySelector('#u-display').value.trim(),
        email: body.querySelector('#u-email').value.trim(),
        password: body.querySelector('#u-password').value,
        role: body.querySelector('#u-role').value,
        groupIds: Array.from(selectedGroupIds),
      };
      if (!payload.username || !payload.password) {
        body.querySelector('.form-error').hidden = false;
        body.querySelector('.form-error').textContent = I18n.t('users.err_required');
        return;
      }
      try {
        await ApiBinary.action('iamCreateUserRequest', payload);
        toast(I18n.t('users.created'), 'success');
        cleanup();
        await loadData(); renderActive();
      } catch (err) {
        body.querySelector('.form-error').hidden = false;
        body.querySelector('.form-error').textContent = err.message || I18n.t('users.save_failed');
      }
    }
  });
}

function openCreateGroupModal() {
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('users.modal_create_group'));
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '440');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `
    <div class="users-form-row full"><div class="field"><tf-input id="g-name" label="${escapeAttr(I18n.t('users.field_group_name'))}" required></tf-input></div></div>
    <div class="users-form-row full"><div class="field"><tf-input id="g-descr" label="${escapeAttr(I18n.t('users.field_group_desc'))}"></tf-input></div></div>
    <div class="form-error" hidden></div>
  `;
  win.appendChild(body);
  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.innerHTML = `<tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button><tf-button variant="primary" data-action="save">${escapeHtml(I18n.t('common.create'))}</tf-button>`;
  win.appendChild(foot);
  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);
  const cleanup = () => { win.remove(); backdrop.remove(); };
  win.addEventListener('action', async (e) => {
    const a = e.detail?.action;
    if (a === 'cancel') return cleanup();
    if (a === 'save') {
      e.preventDefault();
      try {
        await ApiBinary.action('iamCreateGroupRequest', {
          name: body.querySelector('#g-name').value.trim(),
          description: body.querySelector('#g-descr').value.trim(),
        });
        toast(I18n.t('users.group_created'), 'success');
        cleanup();
        await loadData(); renderActive();
      } catch (err) {
        body.querySelector('.form-error').hidden = false;
        body.querySelector('.form-error').textContent = err.message;
      }
    }
  });
}

// ---- Group picker (reusable w create modal + detail tab) ----

function renderGroupPicker(selected) {
  const tags = Array.from(selected)
    .map((gid) => groups.find((g) => g.id === gid))
    .filter(Boolean)
    .map((g) => `<span class="group-tag removable" data-group-id="${g.id}">${escapeHtml(g.name)} <button type="button" class="remove-x" data-remove="${g.id}">×</button></span>`)
    .join('');
  return `
    <div class="group-picker" data-picker>
      ${tags}
      <input class="group-picker-add" type="text" placeholder="${escapeAttr(I18n.t('users.add_group_ph'))}" autocomplete="off">
    </div>
    <div class="group-picker-menu" hidden data-menu></div>
  `;
}

function wireGroupPicker(host, selectedSet) {
  if (!host) return;
  const picker = host.querySelector('[data-picker]');
  const menu = host.querySelector('[data-menu]');
  const input = host.querySelector('.group-picker-add');

  const refresh = () => {
    host.innerHTML = renderGroupPicker(selectedSet);
    wireGroupPicker(host, selectedSet);
  };

  const showMenu = () => {
    const q = (input.value || '').trim().toLowerCase();
    const avail = groups.filter((g) => !selectedSet.has(g.id) && (!q || g.name.toLowerCase().includes(q)));
    if (avail.length === 0) { menu.hidden = true; return; }
    menu.innerHTML = avail.map((g) => `
      <div class="group-option" data-add="${g.id}">
        <div>${escapeHtml(g.name)}</div>
        ${g.description ? `<div class="descr">${escapeHtml(g.description)}</div>` : ''}
      </div>
    `).join('');
    menu.hidden = false;
  };

  input?.addEventListener('focus', showMenu);
  input?.addEventListener('input', showMenu);
  input?.addEventListener('blur', () => { setTimeout(() => { menu.hidden = true; }, 120); });

  picker?.addEventListener('click', (e) => {
    const rm = e.target.closest('[data-remove]');
    if (rm) {
      e.preventDefault();
      selectedSet.delete(Number(rm.dataset.remove));
      refresh();
    }
  });

  menu?.addEventListener('mousedown', (e) => {
    const opt = e.target.closest('[data-add]');
    if (!opt) return;
    e.preventDefault();
    selectedSet.add(Number(opt.dataset.add));
    refresh();
  });
}

// ---- Delete helpers ----

function confirmDeleteUser(u) {
  if (!confirm(I18n.t('users.confirm_delete_user', { name: u.username }))) return;
  ApiBinary.action('iamDeleteUserRequest', { userId: u.id })
    .then(() => { toast(I18n.t('users.deleted'), 'success'); return loadData().then(renderActive); })
    .catch((err) => toast(err.message, 'error'));
}

function confirmDeleteGroup(g) {
  if (!confirm(I18n.t('users.confirm_delete_group', { name: g.name }))) return;
  ApiBinary.action('iamDeleteGroupRequest', { groupId: g.id })
    .then(() => { toast(I18n.t('users.group_deleted'), 'success'); return loadData().then(renderActive); })
    .catch((err) => toast(err.message, 'error'));
}

// ---- Group members + permissions matrix (tab content) ----

async function loadGroupMembers(groupId, host) {
  try {
    const resp = await ApiBinary.action('iamGroupMembersRequest', { groupId });
    const members = resp?.members || [];
    if (members.length === 0) {
      host.innerHTML = `<div class="users-empty">${escapeHtml(I18n.t('users.no_members'))}</div>`;
      return;
    }
    const rows = members.map(renderMemberRow).join('');
    host.innerHTML = `
      <table class="tf-accounts-table">
        <thead>
          <tr>
            <th>${escapeHtml(I18n.t('users.col_user'))}</th>
            <th>${escapeHtml(I18n.t('users.col_role'))}</th>
            <th>${escapeHtml(I18n.t('users.col_source'))}</th>
          </tr>
        </thead>
        <tbody>${rows}</tbody>
      </table>
    `;
  } catch (err) {
    host.innerHTML = `<div style="color:var(--danger); padding:20px;">${escapeHtml(err.message)}</div>`;
  }
}

function renderMemberRow(u) {
  const roleLabel = { admin: I18n.t('users.role_admin'), power_user: I18n.t('users.role_power'), user: I18n.t('users.role_user') }[u.role] || 'User';
  const roleClass = u.role === 'admin' ? 'role-admin' : u.role === 'power_user' ? 'role-power' : 'role-user';
  const sourcePill = u.ssoProvider
    ? `<span class="status-pill sso">SSO · ${escapeHtml(u.ssoProvider)}</span>`
    : `<span class="status-pill local">Local</span>`;
  return `
    <tr>
      <td>
        <div class="tf-account-cell">
          <div class="tf-account-avatar">${escapeHtml(initials(u))}</div>
          <div>
            <div class="tf-account-name">${escapeHtml(u.displayName || u.username)}</div>
            <div class="tf-account-sub">${escapeHtml(u.email || u.username)}</div>
          </div>
        </div>
      </td>
      <td><span class="role-pill ${roleClass}">${escapeHtml(roleLabel)}</span></td>
      <td>${sourcePill}</td>
    </tr>
  `;
}

async function loadGroupPermissions(groupId, host) {
  try {
    const [permsResp, modelsResp, flowsResp, addonsResp] = await Promise.all([
      ApiBinary.action('iamListPermsForSubjectRequest', { subjectType: 'group', subjectId: groupId }),
      ApiBinary.list('modelListRequest').catch(() => []),
      ApiBinary.list('flowListRequest').catch(() => []),
      ApiBinary.list('addonsListRequest', { arrayKey: 'addons' }).catch(() => []),
    ]);
    const entries = permsResp?.entries || [];
    const byResource = {};
    for (const e of entries) {
      byResource[e.resourceType] = byResource[e.resourceType] || {};
      byResource[e.resourceType][e.resourceId] = e.accessLevel;
    }

    const modelItems = (modelsResp || []).map((m) => ({
      id: String(m.id || m.name || m.alias || ''),
      name: String(m.name || m.id || m.alias || ''),
      descr: String(m.description || m.backend || ''),
    }));
    const flowItems = (flowsResp || []).map((f) => ({
      id: String(f.id || f.name || ''),
      name: String(f.name || f.id || ''),
      descr: String(f.description || ''),
    }));
    const addonItems = (addonsResp || []).map((a) => ({
      id: String(a.id || a.addonId || ''),
      name: String(a.name || a.displayName || a.id || ''),
      descr: String(a.description || ''),
    }));

    const renderSection = (label, type, items) => {
      if (items.length === 0) return '';
      const rows = items.map((item) => {
        const current = byResource[type]?.[item.id] || 'auto';
        return renderPermRow(type, item, current, groupId);
      }).join('');
      return `
        <div class="perm-section">
          <div class="perm-section-head"><span>${escapeHtml(label)}</span><span class="counter">${items.length}</span></div>
          ${rows}
        </div>`;
    };

    host.innerHTML = `
      <div class="perm-header-hint">${escapeHtml(I18n.t('users.perm_hint'))}</div>
      ${renderSection(I18n.t('users.perm_models'), 'model', modelItems)}
      ${renderSection(I18n.t('users.perm_flows'), 'flow', flowItems)}
      ${renderSection(I18n.t('users.perm_addons'), 'addon', addonItems)}
    `;

    host.addEventListener('change', async (e) => {
      const seg = e.target.closest('tf-segmented[data-resource-type]');
      if (!seg) return;
      const resourceType = seg.dataset.resourceType;
      const resourceId = seg.dataset.resourceId;
      const level = e.detail?.value;
      try {
        if (level === 'auto') {
          await ApiBinary.action('iamClearPermissionRequest', {
            resourceType, resourceId, subjectType: 'group', subjectId: groupId,
          });
        } else {
          await ApiBinary.action('iamSetPermissionRequest', {
            resourceType, resourceId, subjectType: 'group', subjectId: groupId,
            accessLevel: level,
          });
        }
        const row = seg.closest('.perm-row');
        if (row) row.classList.toggle('denied', level === 'deny');
        toast(I18n.t('users.perm_saved'), 'success');
      } catch (err) {
        toast(err.message || I18n.t('users.perm_save_failed'), 'error');
      }
    });
  } catch (err) {
    host.innerHTML = `<div style="color:var(--tf-danger); padding:20px;">${escapeHtml(err.message)}</div>`;
  }
}

function renderPermRow(resourceType, item, current, _groupId) {
  const isDeny = current === 'deny';
  return `
    <div class="perm-row${isDeny ? ' denied' : ''}">
      <div class="meta">
        <div class="name">${escapeHtml(item.name)}</div>
        ${item.descr ? `<div class="descr">${escapeHtml(item.descr)}</div>` : ''}
      </div>
      <tf-segmented size="sm" value="${escapeAttr(current)}" data-resource-type="${escapeAttr(resourceType)}" data-resource-id="${escapeAttr(item.id)}">
        <option value="auto" variant="neutral">${escapeHtml(I18n.t('users.perm_auto'))}</option>
        <option value="allow" variant="ok">${escapeHtml(I18n.t('users.perm_allow'))}</option>
        <option value="deny" variant="err">${escapeHtml(I18n.t('users.perm_deny'))}</option>
      </tf-segmented>
    </div>
  `;
}

export default UsersScreen;
