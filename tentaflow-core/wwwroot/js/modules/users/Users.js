// =============================================================================
// Plik: modules/users/Users.js
// Opis: Widok zarzadzania uzytkownikami — tabela CRUD, edycja w modalu,
//       zmiana hasla, zarzadzanie grupami. Dostepne tylko dla adminow.
// Przyklad: ViewRouter.register('users', Users);
// =============================================================================

const Users = (() => {
  'use strict';

  let users = [];
  let groups = [];
  let abortController = null;
  let activeModal = null;

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="card">
        <div class="card-header">
          <h3>${I18n.t('users.title') || 'Uzytkownicy'}</h3>
          <button class="btn btn-primary btn-sm" id="btn-add-user">+ ${I18n.t('users.add') || 'Dodaj uzytkownika'}</button>
        </div>
        <div class="card-body no-padding">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>ID</th>
                  <th>Login</th>
                  <th>Nazwa</th>
                  <th>Email</th>
                  <th>Grupy</th>
                  <th>Status</th>
                  <th>Ostatnie logowanie</th>
                  <th>Akcje</th>
                </tr>
              </thead>
              <tbody id="users-tbody">
                <tr><td colspan="8"><div class="empty-state"><div class="empty-state-text">${I18n.t('common.loading') || 'Ladowanie...'}</div></div></td></tr>
              </tbody>
            </table>
          </div>
        </div>
      </div>
      <div class="card" style="margin-top: 16px;">
        <div class="card-header">
          <h3>${I18n.t('groups.title') || 'Grupy'}</h3>
          <button class="btn btn-primary btn-sm" id="btn-add-group">+ ${I18n.t('groups.add') || 'Dodaj grupe'}</button>
        </div>
        <div class="card-body no-padding">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>ID</th>
                  <th>Nazwa</th>
                  <th>Opis</th>
                  <th>Czlonkowie</th>
                  <th>Akcje</th>
                </tr>
              </thead>
              <tbody id="groups-tbody">
                <tr><td colspan="5"><div class="empty-state"><div class="empty-state-text">${I18n.t('common.loading') || 'Ladowanie...'}</div></div></td></tr>
              </tbody>
            </table>
          </div>
        </div>
      </div>
    `;
  }

  // Montowanie
  function mount() {
    abortController = new AbortController();
    const signal = abortController.signal;

    loadData();

    const addUserBtn = document.getElementById('btn-add-user');
    if (addUserBtn) {
      addUserBtn.addEventListener('click', () => openUserModal(null), { signal });
    }

    const addGroupBtn = document.getElementById('btn-add-group');
    if (addGroupBtn) {
      addGroupBtn.addEventListener('click', openGroupModal, { signal });
    }

    const usersTbody = document.getElementById('users-tbody');
    if (usersTbody) {
      usersTbody.addEventListener('click', handleUsersTableClick, { signal });
    }

    const groupsTbody = document.getElementById('groups-tbody');
    if (groupsTbody) {
      groupsTbody.addEventListener('click', handleGroupsTableClick, { signal });
    }
  }

  // Odmontowanie
  function unmount() {
    closeModal();
    if (abortController) {
      abortController.abort();
      abortController = null;
    }
    users = [];
    groups = [];
  }

  // Ladowanie danych
  async function loadData() {
    try {
      const [usersData, groupsData] = await Promise.all([
        ApiClient.get('/api/users'),
        ApiClient.get('/api/groups'),
      ]);
      users = usersData || [];
      groups = groupsData || [];
      renderUsersTable();
      renderGroupsTable();
    } catch (err) {
      App.showToast(`Blad ladowania danych: ${err.message}`, 'error');
    }
  }

  // Renderowanie tabeli uzytkownikow
  function renderUsersTable() {
    const tbody = document.getElementById('users-tbody');
    if (!tbody) return;

    if (users.length === 0) {
      tbody.innerHTML = '<tr><td colspan="8"><div class="empty-state"><div class="empty-state-text">Brak uzytkownikow</div></div></td></tr>';
      return;
    }

    tbody.innerHTML = users.map(user => {
      const userGroups = (user.groups || []).map(g => g.name || g).join(', ') || '-';
      return `
        <tr>
          <td>${user.id}</td>
          <td><strong>${escapeHtml(user.username)}</strong></td>
          <td>${escapeHtml(user.display_name || '-')}</td>
          <td>${escapeHtml(user.email || '-')}</td>
          <td>${escapeHtml(userGroups)}</td>
          <td>
            <span class="status-badge ${user.is_active ? 'status-connected' : 'status-disconnected'}">
              ${user.is_active ? 'Aktywny' : 'Nieaktywny'}
            </span>
          </td>
          <td>${escapeHtml(user.last_login || '-')}</td>
          <td>
            <button class="btn btn-xs btn-ghost" data-action="edit" data-user-id="${user.id}">Edytuj</button>
            <button class="btn btn-xs btn-ghost" data-action="password" data-user-id="${user.id}">Haslo</button>
            <button class="btn btn-xs btn-danger" data-action="delete" data-user-id="${user.id}">Usun</button>
          </td>
        </tr>
      `;
    }).join('');
  }

  // Renderowanie tabeli grup
  function renderGroupsTable() {
    const tbody = document.getElementById('groups-tbody');
    if (!tbody) return;

    if (groups.length === 0) {
      tbody.innerHTML = '<tr><td colspan="5"><div class="empty-state"><div class="empty-state-text">Brak grup</div></div></td></tr>';
      return;
    }

    tbody.innerHTML = groups.map(group => {
      const memberCount = (group.members || []).length;
      return `
        <tr>
          <td>${group.id}</td>
          <td><strong>${escapeHtml(group.name)}</strong></td>
          <td>${escapeHtml(group.description || '-')}</td>
          <td>${memberCount}</td>
          <td>
            <button class="btn btn-xs btn-ghost" data-action="manage-members" data-group-id="${group.id}">${I18n.t('groups.members') || 'Czlonkowie'}</button>
            ${group.is_system ? '' : `<button class="btn btn-xs btn-danger" data-action="delete-group" data-group-id="${group.id}">${I18n.t('common.delete') || 'Usun'}</button>`}
          </td>
        </tr>
      `;
    }).join('');
  }

  // Obsluga klikniec w tabeli uzytkownikow
  async function handleUsersTableClick(e) {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;

    const action = btn.dataset.action;
    const userId = parseInt(btn.dataset.userId, 10);

    switch (action) {
      case 'edit':
        openUserModal(users.find(u => u.id === userId));
        break;
      case 'password':
        openPasswordModal(userId);
        break;
      case 'delete':
        if (confirm('Czy na pewno chcesz usunac tego uzytkownika?')) {
          await deleteUser(userId);
        }
        break;
    }
  }

  // Obsluga klikniec w tabeli grup
  async function handleGroupsTableClick(e) {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;

    const action = btn.dataset.action;
    const groupId = parseInt(btn.dataset.groupId, 10);

    switch (action) {
      case 'manage-members':
        openMembersModal(groupId);
        break;
      case 'delete-group':
        if (confirm(I18n.t('groups.delete_confirm') || 'Czy na pewno chcesz usunac te grupe?')) {
          await deleteGroup(groupId);
        }
        break;
    }
  }

  // Modal dodawania/edycji uzytkownika
  function openUserModal(user) {
    const isEdit = user !== null && user !== undefined;
    const title = isEdit ? 'Edytuj uzytkownika' : 'Nowy uzytkownik';

    const html = `
      <div class="modal-overlay" id="user-modal">
        <div class="modal-content">
          <div class="modal-header">
            <h3>${title}</h3>
            <button class="btn btn-ghost btn-xs modal-close" data-action="close-modal">X</button>
          </div>
          <form id="user-form" class="modal-body">
            <div class="form-group">
              <label for="user-username">Login</label>
              <input type="text" id="user-username" class="form-input" value="${escapeHtml(user?.username || '')}" ${isEdit ? 'readonly' : 'required'}>
            </div>
            ${!isEdit ? `
              <div class="form-group">
                <label for="user-password">Haslo</label>
                <input type="password" id="user-password" class="form-input" required minlength="6" placeholder="Min. 6 znakow">
              </div>
            ` : ''}
            <div class="form-group">
              <label for="user-display-name">Nazwa wyswietlana</label>
              <input type="text" id="user-display-name" class="form-input" value="${escapeHtml(user?.display_name || '')}">
            </div>
            <div class="form-group">
              <label for="user-email">Email</label>
              <input type="email" id="user-email" class="form-input" value="${escapeHtml(user?.email || '')}">
            </div>
            ${isEdit ? `
              <div class="form-group">
                <label>
                  <input type="checkbox" id="user-is-active" ${user.is_active ? 'checked' : ''}>
                  Aktywny
                </label>
              </div>
            ` : ''}
            <div class="modal-footer">
              <button type="button" class="btn btn-ghost" data-action="close-modal">Anuluj</button>
              <button type="submit" class="btn btn-primary">${isEdit ? 'Zapisz' : 'Utworz'}</button>
            </div>
          </form>
        </div>
      </div>
    `;

    document.body.insertAdjacentHTML('beforeend', html);
    activeModal = document.getElementById('user-modal');

    activeModal.querySelector('[data-action="close-modal"]').addEventListener('click', closeModal);
    activeModal.querySelector('.modal-footer [data-action="close-modal"]').addEventListener('click', closeModal);

    activeModal.querySelector('#user-form').addEventListener('submit', async (e) => {
      e.preventDefault();
      if (isEdit) {
        await updateUser(user.id);
      } else {
        await createUser();
      }
    });
  }

  // Tworzenie uzytkownika
  async function createUser() {
    const username = document.getElementById('user-username').value.trim();
    const password = document.getElementById('user-password').value;
    const displayName = document.getElementById('user-display-name').value.trim();
    const email = document.getElementById('user-email').value.trim();

    try {
      await ApiClient.post('/api/users', {
        username,
        password,
        display_name: displayName || null,
        email: email || null,
      });
      App.showToast('Uzytkownik utworzony', 'success');
      closeModal();
      await loadData();
    } catch (err) {
      App.showToast(`Blad: ${err.message}`, 'error');
    }
  }

  // Aktualizacja uzytkownika
  async function updateUser(userId) {
    const displayName = document.getElementById('user-display-name').value.trim();
    const email = document.getElementById('user-email').value.trim();
    const isActive = document.getElementById('user-is-active')?.checked;

    try {
      await ApiClient.put(`/api/users/${userId}`, {
        display_name: displayName || null,
        email: email || null,
        is_active: isActive,
      });
      App.showToast('Uzytkownik zaktualizowany', 'success');
      closeModal();
      await loadData();
    } catch (err) {
      App.showToast(`Blad: ${err.message}`, 'error');
    }
  }

  // Usuniecie uzytkownika
  async function deleteUser(userId) {
    try {
      await ApiClient.delete(`/api/users/${userId}`);
      App.showToast('Uzytkownik usuniety', 'success');
      await loadData();
    } catch (err) {
      App.showToast(`Blad: ${err.message}`, 'error');
    }
  }

  // Modal zmiany hasla
  function openPasswordModal(userId) {
    const html = `
      <div class="modal-overlay" id="password-modal">
        <div class="modal-content">
          <div class="modal-header">
            <h3>Zmiana hasla</h3>
            <button class="btn btn-ghost btn-xs modal-close" data-action="close-modal">X</button>
          </div>
          <form id="password-form" class="modal-body">
            <div class="form-group">
              <label for="new-password">Nowe haslo</label>
              <input type="password" id="new-password" class="form-input" required minlength="6" placeholder="Min. 6 znakow">
            </div>
            <div class="form-group">
              <label for="confirm-password">Potwierdz haslo</label>
              <input type="password" id="confirm-password" class="form-input" required minlength="6">
            </div>
            <div class="modal-footer">
              <button type="button" class="btn btn-ghost" data-action="close-modal">Anuluj</button>
              <button type="submit" class="btn btn-primary">Zmien haslo</button>
            </div>
          </form>
        </div>
      </div>
    `;

    document.body.insertAdjacentHTML('beforeend', html);
    activeModal = document.getElementById('password-modal');

    activeModal.querySelector('[data-action="close-modal"]').addEventListener('click', closeModal);
    activeModal.querySelector('.modal-footer [data-action="close-modal"]').addEventListener('click', closeModal);

    activeModal.querySelector('#password-form').addEventListener('submit', async (e) => {
      e.preventDefault();
      const newPassword = document.getElementById('new-password').value;
      const confirmPassword = document.getElementById('confirm-password').value;

      if (newPassword !== confirmPassword) {
        App.showToast('Hasla nie sa identyczne', 'error');
        return;
      }

      try {
        await ApiClient.put(`/api/users/${userId}/password`, {
          new_password: newPassword,
        });
        App.showToast('Haslo zmienione', 'success');
        closeModal();
      } catch (err) {
        App.showToast(`Blad: ${err.message}`, 'error');
      }
    });
  }

  // Modal zarzadzania czlonkami grupy
  async function openMembersModal(groupId) {
    const group = groups.find(g => g.id === groupId);
    if (!group) return;

    const members = group.members || [];

    // Pobierz liste uzytkownikow do selecta
    let allUsers = [];
    try {
      allUsers = await ApiClient.get('/api/users') || [];
    } catch (err) {
      App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
    }

    // Odfiltruj uzytkownikow juz bedacych czlonkami
    const memberIds = new Set(members.map(m => m.id || m.user_id));
    const availableUsers = allUsers.filter(u => !memberIds.has(u.id));

    const html = `
      <div class="modal-overlay" id="members-modal">
        <div class="modal-content">
          <div class="modal-header">
            <h3>${I18n.t('groups.members') || 'Czlonkowie'}: ${escapeHtml(group.name)}</h3>
            <button class="btn btn-ghost btn-xs modal-close" data-action="close-modal">X</button>
          </div>
          <div class="modal-body">
            <div class="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>ID</th>
                    <th>Login</th>
                    <th>${I18n.t('common.name') || 'Nazwa'}</th>
                    <th>${I18n.t('common.actions') || 'Akcje'}</th>
                  </tr>
                </thead>
                <tbody id="members-tbody">
                  ${members.length === 0
                    ? `<tr><td colspan="4"><div class="empty-state"><div class="empty-state-text">${I18n.t('groups.no_members') || 'Brak czlonkow'}</div></div></td></tr>`
                    : members.map(m => `
                        <tr>
                          <td>${m.id || m.user_id || '-'}</td>
                          <td>${escapeHtml(m.username || '')}</td>
                          <td>${escapeHtml(m.display_name || '-')}</td>
                          <td><button class="btn btn-xs btn-danger" data-action="remove-member" data-user-id="${m.id || m.user_id}">${I18n.t('groups.remove_member') || 'Usun z grupy'}</button></td>
                        </tr>
                      `).join('')
                  }
                </tbody>
              </table>
            </div>
            <div class="inline-form" style="margin-top: 12px; display: flex; gap: 8px; align-items: center;">
              <select id="add-member-select" class="form-input form-input-sm" style="flex: 1;">
                <option value="">${I18n.t('groups.select_user') || 'Wybierz uzytkownika'}</option>
                ${availableUsers.map(u => `<option value="${u.id}">${escapeHtml(u.username)} ${u.display_name ? '(' + escapeHtml(u.display_name) + ')' : ''}</option>`).join('')}
              </select>
              <button class="btn btn-sm btn-primary" data-action="add-member">${I18n.t('groups.add_member') || 'Dodaj'}</button>
            </div>
          </div>
        </div>
      </div>
    `;

    document.body.insertAdjacentHTML('beforeend', html);
    activeModal = document.getElementById('members-modal');

    activeModal.querySelector('[data-action="close-modal"]').addEventListener('click', closeModal);

    activeModal.addEventListener('click', async (e) => {
      const btn = e.target.closest('[data-action]');
      if (!btn) return;

      if (btn.dataset.action === 'add-member') {
        const select = document.getElementById('add-member-select');
        const userId = parseInt(select.value, 10);
        if (isNaN(userId) || !userId) {
          App.showToast(I18n.t('groups.select_user') || 'Wybierz uzytkownika', 'error');
          return;
        }
        try {
          await ApiClient.post(`/api/groups/${groupId}/members`, { user_id: userId });
          App.showToast(I18n.t('groups.member_added') || 'Czlonek dodany', 'success');
          closeModal();
          await loadData();
          // Ponownie otworz modal z odswiezonymi danymi
          openMembersModal(groupId);
        } catch (err) {
          App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
        }
      }

      if (btn.dataset.action === 'remove-member') {
        const userId = parseInt(btn.dataset.userId, 10);
        try {
          await ApiClient.delete(`/api/groups/${groupId}/members/${userId}`);
          App.showToast(I18n.t('groups.member_removed') || 'Czlonek usuniety', 'success');
          closeModal();
          await loadData();
          // Ponownie otworz modal z odswiezonymi danymi
          openMembersModal(groupId);
        } catch (err) {
          App.showToast(`${I18n.t('common.error') || 'Blad'}: ${err.message}`, 'error');
        }
      }
    });
  }

  // Modal tworzenia grupy
  function openGroupModal() {
    const html = `
      <div class="modal-overlay" id="group-modal">
        <div class="modal-content">
          <div class="modal-header">
            <h3>Nowa grupa</h3>
            <button class="btn btn-ghost btn-xs modal-close" data-action="close-modal">X</button>
          </div>
          <form id="group-form" class="modal-body">
            <div class="form-group">
              <label for="group-name">Nazwa grupy</label>
              <input type="text" id="group-name" class="form-input" required>
            </div>
            <div class="form-group">
              <label for="group-desc">Opis</label>
              <input type="text" id="group-desc" class="form-input">
            </div>
            <div class="modal-footer">
              <button type="button" class="btn btn-ghost" data-action="close-modal">Anuluj</button>
              <button type="submit" class="btn btn-primary">Utworz</button>
            </div>
          </form>
        </div>
      </div>
    `;

    document.body.insertAdjacentHTML('beforeend', html);
    activeModal = document.getElementById('group-modal');

    activeModal.querySelector('[data-action="close-modal"]').addEventListener('click', closeModal);
    activeModal.querySelector('.modal-footer [data-action="close-modal"]').addEventListener('click', closeModal);

    activeModal.querySelector('#group-form').addEventListener('submit', async (e) => {
      e.preventDefault();
      const name = document.getElementById('group-name').value.trim();
      const description = document.getElementById('group-desc').value.trim();

      try {
        await ApiClient.post('/api/groups', { name, description: description || null });
        App.showToast('Grupa utworzona', 'success');
        closeModal();
        await loadData();
      } catch (err) {
        App.showToast(`Blad: ${err.message}`, 'error');
      }
    });
  }

  // Usuniecie grupy
  async function deleteGroup(groupId) {
    try {
      await ApiClient.delete(`/api/groups/${groupId}`);
      App.showToast('Grupa usunieta', 'success');
      await loadData();
    } catch (err) {
      App.showToast(`Blad: ${err.message}`, 'error');
    }
  }

  // Zamkniecie modalu
  function closeModal() {
    if (activeModal) {
      activeModal.remove();
      activeModal = null;
    }
  }

  // Escapowanie HTML
  function escapeHtml(str) {
    if (!str) return '';
    const div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
  }

  return { render, mount, unmount };
})();
