// =============================================================================
// Plik: modules/login.js
// Opis: Login screen — username + password → AuthLoginRequest → set JWT, callback.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId } from '/js/utils.js';

const LoginScreen = {
  render() {
    return `
      <div class="login-screen">
        <div class="login-card">
          <div class="login-brand">
            <div class="login-brand-mark">T</div>
            <h1>TentaFlow</h1>
          </div>
          <form id="login-form">
            <div class="form-row">
              <label class="label" for="login-username">Nazwa użytkownika</label>
              <input class="input" id="login-username" type="text" autocomplete="username" required autofocus>
            </div>
            <div class="form-row">
              <label class="label" for="login-password">Hasło</label>
              <input class="input" id="login-password" type="password" autocomplete="current-password" required>
            </div>
            <button class="btn btn-primary btn-lg" type="submit" style="width: 100%;" id="login-submit">
              Zaloguj się
            </button>
            <div id="login-error" class="login-error" style="display: none;"></div>
          </form>
        </div>
      </div>
    `;
  },

  mount({ onSuccess }) {
    const form = byId('login-form');
    const submitBtn = byId('login-submit');
    const errorEl = byId('login-error');

    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      const username = byId('login-username').value.trim();
      const password = byId('login-password').value;
      if (!username || !password) return;

      submitBtn.disabled = true;
      submitBtn.textContent = 'Logowanie…';
      errorEl.style.display = 'none';

      try {
        const result = await ApiBinary.action('authLoginRequest', { username, password });
        if (result.variant === 'AuthLoginResponse' && result.jwt) {
          ApiBinary.setJwt(result.jwt);
          onSuccess();
        } else {
          throw new Error('Niepoprawna odpowiedź serwera');
        }
      } catch (err) {
        errorEl.textContent = err.message ?? 'Błąd logowania';
        errorEl.style.display = 'block';
        submitBtn.disabled = false;
        submitBtn.textContent = 'Zaloguj się';
      }
    });
  },
};

export default LoginScreen;
