// =============================================================================
// Plik: modules/login.js
// Opis: Ekran logowania uzywajacy `authLoginRequest` z protokolu binarnego.
//       Po sukcesie zapisuje JWT i wywoluje callback `onSuccess`. Formularz
//       zbudowany z komponentow tf-* (tf-input, tf-select, tf-button);
//       etykiety pochodza z modulu I18n. Przelacznik jezyka nad formularzem.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml } from '/js/utils.js';
import { I18n, SUPPORTED_LANGS } from '/js/i18n.js';
import FaceBackground from '/js/modules/faceBackground.js';
import { Sfx } from '/js/lib/sfx.js';

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
            <tf-select id="login-lang" title="${escapeHtml(I18n.t('lang.label'))}">
              ${SUPPORTED_LANGS.map((l) => `
                <option value="${l.code}" ${l.code === I18n.getLanguage() ? 'selected' : ''}>${l.flag} ${escapeHtml(l.label)}</option>
              `).join('')}
            </tf-select>
          </div>

          <form id="login-form">
            <tf-input id="login-username" type="text" label="${escapeHtml(I18n.t('login.username'))}" autocomplete="username" required autofocus></tf-input>
            <tf-input id="login-password" type="password" label="${escapeHtml(I18n.t('login.password'))}" autocomplete="current-password" required></tf-input>
            <tf-button id="login-submit" variant="primary" size="md" type="submit" label="${escapeHtml(I18n.t('login.submit'))}"></tf-button>
            <div id="login-error" class="tf-error-text" style="display: none;"></div>
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
      await I18n.setLanguage(e.detail.value);
      const root = byId('app-root');
      root.innerHTML = LoginScreen.render();
      LoginScreen.mount({ onSuccess });
    });

    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      const username = byId('login-username').value.trim();
      const password = byId('login-password').value;
      if (!username || !password) return;

      submitBtn.setAttribute('disabled', '');
      submitBtn.setAttribute('label', `${I18n.t('login.submit')}…`);
      errorEl.style.display = 'none';

      try {
        const result = await ApiBinary.action('authLoginRequest', { username, password });
        if (result.variant === 'AuthLoginResponse' && result.jwt) {
          ApiBinary.setJwt(result.jwt);
          // Kinematograficzne przejście: zoom do oka → reveal UI.
          // Karta logowania fade-outuje przez klasę CSS, UI montujemy
          // w onMidpoint (ok. 1.1 s), face-bg chowa się w onComplete.
          const loginCard = document.querySelector('.login-card');
          if (loginCard) loginCard.classList.add('is-transitioning');
          Sfx.play('login-success');
          FaceBackground.transitionOut({
            onMidpoint: () => onSuccess(),
            onComplete: () => {},
          });
        } else {
          throw new Error(I18n.t('common.error'));
        }
      } catch (err) {
        errorEl.textContent = err.message ?? I18n.t('common.error');
        errorEl.style.display = 'block';
        submitBtn.removeAttribute('disabled');
        submitBtn.setAttribute('label', I18n.t('login.submit'));
        FaceBackground.shakeHead();
        Sfx.play('login-fail');
      }
    });
  },
};

export default LoginScreen;
