// =============================================================================
// Plik: modules/user/UserBinary.js
// Opis: User profile ekran (current user view) zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const UserBinary = (() => {
  'use strict';
  let me = null;

  async function loadMe() {
    try {
      me = await ApiBinary.one('authMeRequest');
      renderMe();
    } catch (err) {
      console.error('[user-binary] me failed:', err);
    }
  }

  function renderMe() {
    const el = document.getElementById('user-detail');
    if (!el || !me) return;
    el.innerHTML = `
      <div class="profile-card">
        <h2>${Utils.escapeHtml(me.username)}</h2>
        <p><strong>${I18n.t('user.role')}:</strong> ${Utils.escapeHtml(me.role)}</p>
      </div>
    `;
  }

  return {
    mount: () => loadMe(),
    unmount: () => { me = null; },
  };
})();

export default UserBinary;
