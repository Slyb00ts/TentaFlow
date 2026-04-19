// =============================================================================
// Plik: modules/login.js
// Opis: Ekran logowania uzywajacy `authLoginRequest` z protokolu binarnego.
//       Po sukcesie zapisuje JWT i wywoluje callback `onSuccess`. Etykiety
//       i komunikaty bledow pochodza z modulu I18n; przelacznik jezyka renderuje
//       sie nad formularzem.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml } from '/js/utils.js';
import { I18n, SUPPORTED_LANGS } from '/js/i18n.js';
import FaceBackground from '/js/modules/faceBackground.js';

const LoginScreen = {
  render() {
    return `
      <div class="login-shell">
        <div class="login-card">
          <div class="login-brand">
            <img src="/tentaflow.png" alt="">
            <div class="name">TentaFlow</div>
            <div style="font-size: 12px; color: var(--text-3);">${escapeHtml(I18n.t('login.tagline'))}</div>
          </div>

          <div style="display: flex; justify-content: flex-end; margin-bottom: 8px;">
            <select id="login-lang" style="padding: 6px 10px; background: var(--bg-input); border: 1px solid var(--border); border-radius: var(--radius-sm); color: var(--text); font-size: 12px;" title="${escapeHtml(I18n.t('lang.label'))}">
              ${SUPPORTED_LANGS.map((l) => `
                <option value="${l.code}" ${l.code === I18n.getLanguage() ? 'selected' : ''}>${l.flag} ${escapeHtml(l.label)}</option>
              `).join('')}
            </select>
          </div>

          <form id="login-form">
            <label for="login-username">${escapeHtml(I18n.t('login.username'))}</label>
            <input id="login-username" type="text" autocomplete="username" required autofocus>
            <label for="login-password">${escapeHtml(I18n.t('login.password'))}</label>
            <input id="login-password" type="password" autocomplete="current-password" required>
            <button class="btn btn-primary login-submit" type="submit" id="login-submit">
              ${escapeHtml(I18n.t('login.submit'))}
            </button>
            <div id="login-error" class="login-error" style="display: none;"></div>
          </form>
        </div>
      </div>
    `;
  },

  mount({ onSuccess }) {
    FaceBackground.show();

    const form = byId('login-form');
    const submitBtn = byId('login-submit');
    const errorEl = byId('login-error');

    byId('login-lang')?.addEventListener('change', async (e) => {
      await I18n.setLanguage(e.target.value);
      const root = byId('app-root');
      root.innerHTML = LoginScreen.render();
      LoginScreen.mount({ onSuccess });
    });

    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      const username = byId('login-username').value.trim();
      const password = byId('login-password').value;
      if (!username || !password) return;

      submitBtn.disabled = true;
      submitBtn.textContent = `${I18n.t('login.submit')}…`;
      errorEl.style.display = 'none';

      try {
        const result = await ApiBinary.action('authLoginRequest', { username, password });
        if (result.variant === 'AuthLoginResponse' && result.jwt) {
          ApiBinary.setJwt(result.jwt);
          FaceBackground.hide();
          onSuccess();
        } else {
          throw new Error(I18n.t('common.error'));
        }
      } catch (err) {
        errorEl.textContent = err.message ?? I18n.t('common.error');
        errorEl.style.display = 'block';
        submitBtn.disabled = false;
        submitBtn.textContent = I18n.t('login.submit');
      }
    });
  },
};

export default LoginScreen;
