// =============================================================================
// Plik: modules/users.js
// Opis: Admin screen: lista userow + grupy. Wymaga role=admin zalogowanego.
//       Tabs Users / Groups. Row click → edycja w modal.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast, patchInner } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let me = null;
let users = [];
let groups = [];
let activeTab = 'users'; // 'users' | 'groups'
let filter = 'all';
let searchQuery = '';

const UsersScreen = {
  title: 'Użytkownicy',
  render() {
    return `
      <div class="page users-screen">
        <div class="page-header">
          <div>
            <h1>${escapeHtml(I18n.t('users.title'))}</h1>
            <div class="sub" id="users-sub"></div>
          </div>
          <div class="actions" id="users-actions"></div>
        </div>
        <div class="mesh-tabs">
          <tf-tabs variant="soft" value="users" id="users-tabs-nav">
            <tf-tab id="users">${escapeHtml(I18n.t('users.tab_users'))}</tf-tab>
            <tf-tab id="groups">${escapeHtml(I18n.t('users.tab_groups'))}</tf-tab>
          </tf-tabs>
        </div>
        <div id="users-content"><div class="mesh-loading">${escapeHtml(I18n.t('common.loading'))}</div></div>
      </div>
    `;
  },
  async mount() {
    try {
      me = await ApiBinary.one('authMeRequest');
    } catch { me = null; }
    if (!me || (me.role !== 'admin' && !me.isAdmin)) {
      byId('users-content').innerHTML = `<div class="card"><p>${escapeHtml(I18n.t('users.admin_only'))}</p></div>`;
      return;
    }
    const tabsEl = byId('users-tabs-nav');
    if (tabsEl) tabsEl.addEventListener('change', (e) => {
      activeTab = e.detail?.value || 'users';
      renderActive();
    });
    const contentEl = byId('users-content');
    if (contentEl) contentEl.addEventListener('click', handleClick);
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
  const sub = byId('users-sub');
  const actions = byId('users-actions');
  const host = byId('users-content');
  if (!host) return;
  if (activeTab === 'users') {
    if (sub) sub.textContent = I18n.t('users.count_users', { n: users.length });
    if (actions) actions.innerHTML = `<tf-button variant="primary" icon="plus" id="btn-add-user">${escapeHtml(I18n.t('users.new_user'))}</tf-button>`;
    actions?.querySelector('#btn-add-user')?.addEventListener('click', openCreateUserModal);
    patchInner(host, renderUsersList());
  } else {
    if (sub) sub.textContent = I18n.t('users.count_groups', { n: groups.length });
    if (actions) actions.innerHTML = `<tf-button variant="primary" icon="plus" id="btn-add-group">${escapeHtml(I18n.t('users.new_group'))}</tf-button>`;
    actions?.querySelector('#btn-add-group')?.addEventListener('click', openCreateGroupModal);
    patchInner(host, renderGroupsList());
  }
}

function renderUsersList() {
  if (users.length === 0) {
    return `<div class="card empty-state"><p>${escapeHtml(I18n.t('users.no_users'))}</p></div>`;
  }
  const rows = users.map(renderUserRow).join('');
  return `
    <div class="addons-toolbar">
      <tf-searchbox id="users-search" placeholder="${escapeAttr(I18n.t('users.search_ph'))}" debounce="120"></tf-searchbox>
      <div class="tf-filter-group">
        <tf-chip class="filter-chip" clickable ${filter === 'all' ? 'active' : ''} data-filter="all">${escapeHtml(I18n.t('users.filter_all'))}</tf-chip>
        <tf-chip class="filter-chip" clickable ${filter === 'active' ? 'active' : ''} data-filter="active">${escapeHtml(I18n.t('users.filter_active'))}</tf-chip>
        <tf-chip class="filter-chip" clickable ${filter === 'admin' ? 'active' : ''} data-filter="admin">${escapeHtml(I18n.t('users.filter_admin'))}</tf-chip>
        <tf-chip class="filter-chip" clickable ${filter === 'inactive' ? 'active' : ''} data-filter="inactive">${escapeHtml(I18n.t('users.filter_inactive'))}</tf-chip>
      </div>
    </div>
    <div class="user-list">${rows}</div>
  `;
}

function renderUserRow(u) {
  const initials = (u.displayName || u.username || '?').split(/\s+/).map((p) => p[0]).slice(0, 2).join('').toUpperCase();
  const roleLabel = { admin: I18n.t('users.role_admin'), power_user: I18n.t('users.role_power'), user: I18n.t('users.role_user') }[u.role] || u.role || 'user';
  const roleChip = u.role === 'admin' ? 'admin' : u.role === 'power_user' ? 'accent' : 'offline';
  const statusChip = u.isActive ? '<tf-chip status="ok" dot>aktywny</tf-chip>' : '<tf-chip status="offline" dot>nieaktywny</tf-chip>';
  const src = u.ssoProvider ? `SSO · ${escapeHtml(u.ssoProvider)}` : 'Local';
  const groupTags = (u.groupIds || [])
    .map((gid) => groups.find((g) => g.id === gid))
    .filter(Boolean)
    .map((g) => `<span class="group-tag">${escapeHtml(g.name)}</span>`)
    .join('');
  return `
    <div class="user-row" data-user-id="${u.id}">
      <div class="user-avatar">${escapeHtml(initials)}</div>
      <div class="user-main">
        <div class="user-title">${escapeHtml(u.displayName || u.username)} ${statusChip}</div>
        <div class="user-sub">${escapeHtml(u.email || '')} · ${escapeHtml(src)}</div>
        <div class="user-groups">${groupTags || `<span style="color:var(--text-3); font-size:11px;">${escapeHtml(I18n.t('users.no_groups'))}</span>`}</div>
      </div>
      <div class="user-role-col">
        <tf-chip status="${roleChip}">${escapeHtml(roleLabel)}</tf-chip>
      </div>
      <div class="user-row-actions">
        <tf-button variant="ghost" size="sm" icon="edit" data-action="edit-user" title="${escapeAttr(I18n.t('users.edit'))}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="trash" data-action="delete-user" title="${escapeAttr(I18n.t('users.delete'))}"></tf-button>
      </div>
    </div>
  `;
}

function renderGroupsList() {
  if (groups.length === 0) {
    return `<div class="card empty-state"><p>${escapeHtml(I18n.t('users.no_groups_yet'))}</p></div>`;
  }
  const rows = groups.map(renderGroupRow).join('');
  return `<div class="user-list">${rows}</div>`;
}

function renderGroupRow(g) {
  return `
    <div class="user-row" data-group-id="${g.id}">
      <div class="user-avatar" style="background:linear-gradient(135deg,#a78bfa,#6366f1);"><svg viewBox="0 0 24 24" style="width:20px;height:20px;stroke:white;fill:none;stroke-width:2;stroke-linecap:round;stroke-linejoin:round;"><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 0 0-3-3.87"/></svg></div>
      <div class="user-main">
        <div class="user-title">${escapeHtml(g.name)}</div>
        <div class="user-sub">${escapeHtml(g.description || '')}</div>
        <div class="user-groups"><span style="color:var(--text-3); font-size:11px;">${escapeHtml(I18n.t('users.members_count', { n: g.memberCount || 0 }))}</span></div>
      </div>
      <div></div>
      <div class="user-row-actions">
        <tf-button variant="ghost" size="sm" icon="edit" data-action="edit-group" title="${escapeAttr(I18n.t('users.edit'))}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="trash" data-action="delete-group" title="${escapeAttr(I18n.t('users.delete'))}"></tf-button>
      </div>
    </div>
  `;
}

function handleClick(e) {
  const row = e.target.closest('[data-user-id]');
  const groupRow = e.target.closest('[data-group-id]');
  const actionBtn = e.target.closest('[data-action]');
  if (actionBtn) {
    e.stopPropagation();
    const action = actionBtn.dataset.action;
    if (action === 'edit-user' && row) {
      const u = users.find((x) => String(x.id) === row.dataset.userId);
      if (u) openEditUserModal(u);
    } else if (action === 'delete-user' && row) {
      const u = users.find((x) => String(x.id) === row.dataset.userId);
      if (u) confirmDeleteUser(u);
    } else if (action === 'edit-group' && groupRow) {
      const g = groups.find((x) => String(x.id) === groupRow.dataset.groupId);
      if (g) openEditGroupModal(g);
    } else if (action === 'delete-group' && groupRow) {
      const g = groups.find((x) => String(x.id) === groupRow.dataset.groupId);
      if (g) confirmDeleteGroup(g);
    }
    return;
  }
  const chipFilter = e.target.closest('[data-filter]');
  if (chipFilter) {
    filter = chipFilter.dataset.filter;
    renderActive();
    return;
  }
  if (row) {
    const u = users.find((x) => String(x.id) === row.dataset.userId);
    if (u) openEditUserModal(u);
  } else if (groupRow) {
    const g = groups.find((x) => String(x.id) === groupRow.dataset.groupId);
    if (g) openEditGroupModal(g);
  }
}

// ---- Modals ----

function openCreateUserModal() {
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('users.modal_create_user'));
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '480');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `
    <div class="form-row"><tf-input id="u-username" label="${escapeAttr(I18n.t('users.field_username'))}" required></tf-input></div>
    <div class="form-row"><tf-input id="u-display" label="${escapeAttr(I18n.t('users.field_display'))}"></tf-input></div>
    <div class="form-row"><tf-input id="u-email" type="email" label="${escapeAttr(I18n.t('users.field_email'))}"></tf-input></div>
    <div class="form-row"><tf-input id="u-password" type="password" label="${escapeAttr(I18n.t('users.field_password'))}" required></tf-input></div>
    <div class="form-row">
      <tf-select id="u-role" label="${escapeAttr(I18n.t('users.field_role'))}">
        <option value="user">${escapeHtml(I18n.t('users.role_user_desc'))}</option>
        <option value="power_user">${escapeHtml(I18n.t('users.role_power_desc'))}</option>
        <option value="admin">${escapeHtml(I18n.t('users.role_admin_desc'))}</option>
      </tf-select>
    </div>
    <div class="form-row"><label style="font-size:10px; text-transform:uppercase; letter-spacing:0.04em; font-weight:700; color:var(--tf-text-2); margin-bottom:6px;">${escapeHtml(I18n.t('users.field_groups'))}</label>
      <div id="u-groups" class="group-picker">${renderGroupCheckboxes(new Set())}</div>
    </div>
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
      const payload = {
        username: body.querySelector('#u-username').value.trim(),
        displayName: body.querySelector('#u-display').value.trim(),
        email: body.querySelector('#u-email').value.trim(),
        password: body.querySelector('#u-password').value,
        role: body.querySelector('#u-role').value,
        groupIds: Array.from(body.querySelectorAll('#u-groups input:checked')).map((cb) => Number(cb.value)),
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

function openEditUserModal(u) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', `${I18n.t('users.modal_edit_user')} — ${u.displayName || u.username}`);
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const currentGroups = new Set(u.groupIds || []);
  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `
    <div class="form-row"><tf-input id="u-display" label="${escapeAttr(I18n.t('users.field_display'))}" value="${escapeAttr(u.displayName || '')}"></tf-input></div>
    <div class="form-row"><tf-input id="u-email" type="email" label="${escapeAttr(I18n.t('users.field_email'))}" value="${escapeAttr(u.email || '')}"></tf-input></div>
    <div class="form-row">
      <tf-select id="u-role" label="${escapeAttr(I18n.t('users.field_role'))}" value="${escapeAttr(u.role || 'user')}">
        <option value="user">${escapeHtml(I18n.t('users.role_user_desc'))}</option>
        <option value="power_user">${escapeHtml(I18n.t('users.role_power_desc'))}</option>
        <option value="admin">${escapeHtml(I18n.t('users.role_admin_desc'))}</option>
      </tf-select>
    </div>
    <div class="form-row">
      <tf-toggle id="u-active" ${u.isActive ? 'checked' : ''}>${escapeHtml(I18n.t('users.field_active'))}</tf-toggle>
    </div>
    <div class="form-row"><label style="font-size:10px; text-transform:uppercase; letter-spacing:0.04em; font-weight:700; color:var(--tf-text-2); margin-bottom:6px;">${escapeHtml(I18n.t('users.field_groups'))}</label>
      <div id="u-groups" class="group-picker">${renderGroupCheckboxes(currentGroups)}</div>
    </div>
    <div class="form-row">
      <tf-button variant="secondary" icon="key" data-action="reset-pw" style="width:100%;">${escapeHtml(I18n.t('users.reset_password'))}</tf-button>
    </div>
    <div class="form-error" hidden></div>
  `;
  win.appendChild(body);
  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.innerHTML = `<tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button><tf-button variant="primary" data-action="save">${escapeHtml(I18n.t('common.save'))}</tf-button>`;
  win.appendChild(foot);
  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);
  const cleanup = () => { win.remove(); backdrop.remove(); };
  win.addEventListener('action', async (e) => {
    const a = e.detail?.action;
    if (a === 'cancel') return cleanup();
    if (a === 'reset-pw') {
      const newPw = prompt(I18n.t('users.reset_prompt'));
      if (!newPw) return;
      try {
        await ApiBinary.action('iamResetUserPasswordRequest', { userId: u.id, newPassword: newPw });
        toast(I18n.t('users.password_reset'), 'success');
      } catch (err) { toast(err.message, 'error'); }
      return;
    }
    if (a === 'save') {
      e.preventDefault();
      try {
        await ApiBinary.action('iamUpdateUserRequest', {
          userId: u.id,
          displayName: body.querySelector('#u-display').value.trim(),
          email: body.querySelector('#u-email').value.trim(),
          isActive: body.querySelector('#u-active').hasAttribute('checked'),
          role: body.querySelector('#u-role').value,
        });
        const newGroupIds = Array.from(body.querySelectorAll('#u-groups input:checked')).map((cb) => Number(cb.value));
        await ApiBinary.action('iamSetUserGroupsRequest', { userId: u.id, groupIds: newGroupIds });
        toast(I18n.t('users.saved'), 'success');
        cleanup();
        await loadData(); renderActive();
      } catch (err) {
        body.querySelector('.form-error').hidden = false;
        body.querySelector('.form-error').textContent = err.message;
      }
    }
  });
}

function renderGroupCheckboxes(selected) {
  if (groups.length === 0) return `<div style="color:var(--tf-text-3); font-size:12px;">${escapeHtml(I18n.t('users.no_groups_yet'))}</div>`;
  return groups.map((g) => `
    <label style="display:flex; align-items:center; gap:8px; padding:6px 10px; background:var(--tf-bg-input); border:1px solid var(--tf-border); border-radius:8px; margin-bottom:4px; cursor:pointer;">
      <input type="checkbox" value="${g.id}" ${selected.has(g.id) ? 'checked' : ''} style="accent-color:var(--tf-accent-1);">
      <span>${escapeHtml(g.name)}</span>
      ${g.description ? `<span style="color:var(--tf-text-3); font-size:11px;">· ${escapeHtml(g.description)}</span>` : ''}
    </label>
  `).join('');
}

function confirmDeleteUser(u) {
  if (!confirm(I18n.t('users.confirm_delete_user', { name: u.username }))) return;
  ApiBinary.action('iamDeleteUserRequest', { userId: u.id })
    .then(() => { toast(I18n.t('users.deleted'), 'success'); return loadData().then(renderActive); })
    .catch((err) => toast(err.message, 'error'));
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
    <div class="form-row"><tf-input id="g-name" label="${escapeAttr(I18n.t('users.field_group_name'))}" required></tf-input></div>
    <div class="form-row"><tf-input id="g-descr" label="${escapeAttr(I18n.t('users.field_group_desc'))}"></tf-input></div>
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

async function openEditGroupModal(g) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', `${I18n.t('users.modal_edit_group')} — ${g.name}`);
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '720');
  win.setAttribute('min-width', '560');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `
    <tf-tabs variant="underline" value="info" id="g-tabs">
      <tf-tab id="info">${escapeHtml(I18n.t('users.tab_info'))}</tf-tab>
      <tf-tab id="members">${escapeHtml(I18n.t('users.tab_members'))}</tf-tab>
      <tf-tab id="perms">${escapeHtml(I18n.t('users.tab_perms'))}</tf-tab>
    </tf-tabs>
    <div class="g-panel" data-tab="info">
      <div class="form-row"><tf-input id="g-name" label="${escapeAttr(I18n.t('users.field_group_name'))}" value="${escapeAttr(g.name || '')}"></tf-input></div>
      <div class="form-row"><tf-input id="g-descr" label="${escapeAttr(I18n.t('users.field_group_desc'))}" value="${escapeAttr(g.description || '')}"></tf-input></div>
    </div>
    <div class="g-panel" data-tab="members" hidden>
      <div id="g-members-list" class="user-list"><div class="mesh-loading">${escapeHtml(I18n.t('common.loading'))}</div></div>
    </div>
    <div class="g-panel" data-tab="perms" hidden>
      <div id="g-perms-body"><div class="mesh-loading">${escapeHtml(I18n.t('common.loading'))}</div></div>
    </div>
    <div class="form-error" hidden></div>
  `;
  win.appendChild(body);
  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.innerHTML = `<tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button><tf-button variant="primary" data-action="save">${escapeHtml(I18n.t('common.save'))}</tf-button>`;
  win.appendChild(foot);
  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);
  const cleanup = () => { win.remove(); backdrop.remove(); };

  const tabsEl = body.querySelector('#g-tabs');
  const panels = body.querySelectorAll('.g-panel');
  let membersLoaded = false;
  let permsLoaded = false;
  tabsEl.addEventListener('change', async (e) => {
    const val = e.detail?.value;
    panels.forEach((p) => { p.hidden = p.dataset.tab !== val; });
    if (val === 'members' && !membersLoaded) {
      await loadGroupMembers(g.id, body.querySelector('#g-members-list'));
      membersLoaded = true;
    } else if (val === 'perms' && !permsLoaded) {
      await loadGroupPermissions(g.id, body.querySelector('#g-perms-body'));
      permsLoaded = true;
    }
  });

  win.addEventListener('action', async (e) => {
    const a = e.detail?.action;
    if (a === 'cancel') return cleanup();
    if (a === 'save') {
      e.preventDefault();
      try {
        await ApiBinary.action('iamUpdateGroupRequest', {
          groupId: g.id,
          name: body.querySelector('#g-name').value.trim(),
          description: body.querySelector('#g-descr').value.trim(),
        });
        toast(I18n.t('users.group_saved'), 'success');
        cleanup();
        await loadData(); renderActive();
      } catch (err) {
        body.querySelector('.form-error').hidden = false;
        body.querySelector('.form-error').textContent = err.message;
      }
    }
  });
}

async function loadGroupMembers(groupId, host) {
  try {
    const resp = await ApiBinary.action('iamGroupMembersRequest', { groupId });
    const members = resp?.members || [];
    if (members.length === 0) {
      host.innerHTML = `<div style="color:var(--tf-text-3); padding:20px; text-align:center;">${escapeHtml(I18n.t('users.no_members'))}</div>`;
      return;
    }
    host.innerHTML = members.map(renderUserRow).join('');
  } catch (err) {
    host.innerHTML = `<div style="color:var(--tf-danger); padding:20px;">${escapeHtml(err.message)}</div>`;
  }
}

// ---- Permissions matrix per grupa ----
// Dla kazdego typu zasobu (model/flow/addon) pokazujemy wszystkie dostepne
// zasoby + segmented toggle Auto/Zezwól/Odmów. Auto = brak wpisu (default allow).
async function loadGroupPermissions(groupId, host) {
  try {
    const [permsResp, modelsResp, flowsResp, addonsResp] = await Promise.all([
      ApiBinary.action('iamListPermsForSubjectRequest', { subjectType: 'group', subjectId: groupId }),
      ApiBinary.list('modelListRequest').catch(() => []),
      ApiBinary.list('flowListRequest').catch(() => []),
      ApiBinary.list('addonsListRequest', { arrayKey: 'addons' }).catch(() => []),
    ]);
    const entries = permsResp?.entries || [];
    // Zmapuj per resource_type → resource_id → access_level.
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

function confirmDeleteGroup(g) {
  if (!confirm(I18n.t('users.confirm_delete_group', { name: g.name }))) return;
  ApiBinary.action('iamDeleteGroupRequest', { groupId: g.id })
    .then(() => { toast(I18n.t('users.group_deleted'), 'success'); return loadData().then(renderActive); })
    .catch((err) => toast(err.message, 'error'));
}

export default UsersScreen;
