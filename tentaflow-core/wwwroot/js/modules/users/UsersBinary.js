// =============================================================================
// Plik: modules/users/UsersBinary.js
// Opis: Users / current user info zmigrowany na binary protocol.
//       Bootstrap pokrywa AuthMe (current logged user); user list/CRUD
//       wymaga wlasnych UserListRequest variantow ktore dodamy w phase 2.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const UsersBinary = (() => {
  'use strict';
  let currentUser = null;

  async function loadCurrentUser() {
    try {
      const body = await ApiBinary.one('authMeRequest');
      // body.variant === 'AuthMeResponse'
      currentUser = body;
      renderProfile();
    } catch (err) {
      console.error('[users-binary] me failed:', err);
    }
  }

  function renderProfile() {
    const el = document.getElementById('user-profile');
    if (!el || !currentUser) return;
    el.innerHTML = `
      <div class="profile-card">
        <h3>${Utils.escapeHtml(currentUser.username)}</h3>
        <p>${I18n.t('users.role')}: <span class="badge">${Utils.escapeHtml(currentUser.role)}</span></p>
      </div>
    `;
  }

  return {
    mount: () => loadCurrentUser(),
    unmount: () => { currentUser = null; },
  };
})();

export default UsersBinary;
