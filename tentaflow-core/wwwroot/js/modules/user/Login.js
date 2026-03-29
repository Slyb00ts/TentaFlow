// =============================================================================
// Plik: modules/user/Login.js
// Opis: Obsluga formularza logowania - walidacja, submit do API, bledy.
// Przyklad: Login.init(onSuccessCallback);
// =============================================================================

const Login = (() => {
  'use strict';

  let onSuccess = null;

  // Inicjalizacja - podlaczenie formularza
  function init(successCallback) {
    onSuccess = successCallback;
    const form = document.getElementById('login-form');
    if (form) {
      form.addEventListener('submit', handleSubmit);
    }
  }

  // Obsluga wysylania formularza
  async function handleSubmit(e) {
    e.preventDefault();

    const username = document.getElementById('username').value.trim();
    const password = document.getElementById('password').value;
    const errorEl = document.getElementById('login-error');
    const submitBtn = e.target.querySelector('button[type="submit"]');

    // Ukryj poprzedni blad
    if (errorEl) errorEl.hidden = true;

    // Walidacja
    if (!username || !password) {
      showError(I18n.t('login.username') + ' & ' + I18n.t('login.password'));
      return;
    }

    // Zablokuj przycisk
    if (submitBtn) {
      submitBtn.disabled = true;
      submitBtn.textContent = '...';
    }

    try {
      const result = await ApiClient.login(username, password);
      if (onSuccess) onSuccess();
    } catch (err) {
      showError(err.message || I18n.t('login.submit'));
    } finally {
      if (submitBtn) {
        submitBtn.disabled = false;
        submitBtn.textContent = I18n.t('login.submit');
      }
    }
  }

  // Wyswietlenie bledu
  function showError(message) {
    const errorEl = document.getElementById('login-error');
    if (errorEl) {
      errorEl.textContent = message;
      errorEl.hidden = false;
    }
  }

  return { init };
})();
