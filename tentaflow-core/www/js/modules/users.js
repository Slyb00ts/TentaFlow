// =============================================================================
// Plik: modules/users.js
// Opis: Profil zalogowanego usera (AuthMeRequest).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, shortHex, toast } from '/js/utils.js';

const UsersScreen = {
  title: 'Użytkownicy',
  render() {
    return `
      <div class="content-header"><h1>Użytkownicy</h1></div>
      <div id="users-host"></div>`;
  },
  async mount() {
    try {
      const me = await ApiBinary.one('authMeRequest');
      byId('users-host').innerHTML = `
        <div class="card" style="max-width: 480px;">
          <div class="card-header">
            <h3 class="card-title">Twój profil</h3>
            <tf-chip status="accent">${escapeHtml(me.role)}</tf-chip>
          </div>
          <div class="form-row"><span class="label">Username</span>
            <div style="font-size: var(--text-lg);">${escapeHtml(me.username)}</div></div>
          <div class="form-row"><span class="label">User ID</span>
            <div><code>${shortHex(me.userId, 16)}…</code></div></div>
          <p style="color: var(--color-text-muted); font-size: var(--text-sm); margin-top: var(--space-4);">
            Lista wszystkich użytkowników wymaga oddzielnego endpointu UserListRequest — dodajemy w kolejnym sprincie.
          </p>
        </div>`;
    } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
  },
  unmount() {},
};

export default UsersScreen;
