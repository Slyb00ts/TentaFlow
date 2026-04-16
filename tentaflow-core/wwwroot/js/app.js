// =============================================================================
// Plik: app.js
// Opis: Glowna klasa aplikacji - inicjalizacja, autentykacja, nawigacja,
//       obsluga toastow i lifecycle SPA.
// Przyklad: App.init() - uruchom po zaladowaniu DOM.
// =============================================================================

// Widok regul z zakladkami (PII, Fast-path, TTS)
const RulesView = (() => {
  'use strict';

  let activeTab = 'pii';
  let activeModule = null;

  // Mapowanie zakladek na moduly
  const TAB_MODULES = {
    pii: PiiRules,
    'fast-path': FastPathPatterns,
    tts: TtsCleaningRules,
  };

  // Renderowanie glownego kontenera z zakladkami
  function render() {
    return `
      <div class="rules-tabs">
        <button class="rules-tab active" data-rules-tab="pii" data-i18n="rules.tabs.pii">${I18n.t('rules.tabs.pii')}</button>
        <button class="rules-tab" data-rules-tab="fast-path" data-i18n="rules.tabs.fast_path">${I18n.t('rules.tabs.fast_path')}</button>
        <button class="rules-tab" data-rules-tab="tts" data-i18n="rules.tabs.tts">${I18n.t('rules.tabs.tts')}</button>
      </div>
      <div id="rules-content"></div>
    `;
  }

  // Montowanie - podepnij zakladki, zaladuj domyslna
  function mount() {
    const content = document.getElementById('content');
    if (!content) return;

    content.querySelectorAll('.rules-tab').forEach(tab => {
      tab.addEventListener('click', () => {
        switchTab(tab.dataset.rulesTab);
      });
    });

    // Zaladuj domyslna zakladke
    switchTab('pii');
  }

  // Przelaczenie zakladki
  function switchTab(tabId) {
    // Odmontuj aktualny modul
    if (activeModule && typeof activeModule.unmount === 'function') {
      activeModule.unmount();
    }

    activeTab = tabId;

    // Aktualizuj zaznaczenie zakladek
    const content = document.getElementById('content');
    if (content) {
      content.querySelectorAll('.rules-tab').forEach(t => {
        t.classList.toggle('active', t.dataset.rulesTab === tabId);
      });
    }

    // Renderuj wybrany modul do #rules-content
    const rulesContent = document.getElementById('rules-content');
    if (!rulesContent) return;

    activeModule = TAB_MODULES[tabId];
    if (activeModule) {
      rulesContent.innerHTML = activeModule.render();
      if (typeof activeModule.mount === 'function') {
        activeModule.mount();
      }
    }
  }

  // Odmontowanie
  function unmount() {
    if (activeModule && typeof activeModule.unmount === 'function') {
      activeModule.unmount();
    }
    activeModule = null;
    activeTab = 'pii';
  }

  return { render, mount, unmount };
})();

const App = (() => {
  'use strict';

  let statusHandler = null;
  let routerInitialized = false;
  let toastContainerRef = null;

  // Ograniczenie toastow: max 5 na sekunde
  const TOAST_RATE_LIMIT = 5;
  const TOAST_RATE_WINDOW = 1000;
  let toastTimestamps = [];

  // Inicjalizacja aplikacji
  async function init() {
    console.log('[App] init start');

    // Inicjalizuj i18n przed resztą, żeby labelki były poprawne
    await I18n.init();

    // Zarejestruj widoki w routerze
    ViewRouter.register('dashboard', Dashboard);
    ViewRouter.register('services', Services);
    ViewRouter.register('apikeys', ApiKeys);
    ViewRouter.register('settings', Settings);
    ViewRouter.register('mesh', Mesh);
    ViewRouter.register('clusters', Clusters);
    ViewRouter.register('prompts', Prompts);
    ViewRouter.register('models', Models);
    ViewRouter.register('rules', RulesView);
    ViewRouter.register('registries', Registries);
    ViewRouter.register('flows', FlowList);
    ViewRouter.register('chat', Chat);
    ViewRouter.register('addons', Addons);
    ViewRouter.register('meeting', MeetingBot);
    ViewRouter.register('users', Users);
    ViewRouter.register('audit', AuditLog);

    // Inicjalizuj formularz logowania
    Login.init(onLoginSuccess);

    // Obsluga wylogowania
    const logoutBtn = document.getElementById('btn-logout');
    if (logoutBtn) {
      logoutBtn.addEventListener('click', handleLogout);
    }

    // Nasluchiwanie na wygasniecie sesji
    window.addEventListener('auth:expired', () => {
      console.warn('[App] auth:expired event!');
      showLoginScreen();
      showToast(I18n.t('login.error_session_expired'), 'error');
    });

    // Sprawdz czy jest token
    if (ApiClient.hasToken() && !ApiClient.isTokenExpired()) {
      console.log('[App] token istnieje i nie wygasl -> onLoginSuccess');
      onLoginSuccess();
    } else {
      console.log('[App] brak tokenu lub wygasl -> showLoginScreen');
      showLoginScreen();
    }

    // Inicjalizuj kontener toastow
    initToastContainer();
  }

  // Po udanym zalogowaniu
  function onLoginSuccess() {
    console.log('[App] onLoginSuccess wywolany');
    // Wyczysc cache licencji - moze byc to inny user niz poprzednio
    if (typeof LicenseBadge !== 'undefined' && typeof LicenseBadge.invalidate === 'function') {
      LicenseBadge.invalidate();
    }
    showAppScreen();

    // Ustaw nazwe uzytkownika
    const userDisplay = document.getElementById('user-display');
    if (userDisplay) {
      userDisplay.textContent = ApiClient.getUsername() || 'admin';
    }

    // Inicjalizuj router nawigacji (tylko raz - listenery na statycznych elementach sidebar)
    if (!routerInitialized) {
      ViewRouter.init();
      routerInitialized = true;
    }

    // Polacz WebSocket
    WsClient.connect();

    // Usun poprzedni listener statusu WS (zapobieganie kumulacji przy re-logowaniu)
    if (statusHandler) {
      WsClient.off('status', statusHandler);
    }
    statusHandler = (status) => {
      updateConnectionStatus(status);
    };
    WsClient.on('status', statusHandler);

    // Zaladuj dashboard
    console.log('[App] navigating to dashboard...');
    ViewRouter.navigate('dashboard');
    console.log('[App] onLoginSuccess finished');
  }

  // Wylogowanie
  function handleLogout() {
    ApiClient.logout();
    WsClient.disconnect();
    // Wyczysc cache licencji - kolejny user moze miec inny tier
    if (typeof LicenseBadge !== 'undefined' && typeof LicenseBadge.invalidate === 'function') {
      LicenseBadge.invalidate();
    }
    showLoginScreen();
  }

  // Pokaz ekran logowania, ukryj aplikacje
  function showLoginScreen() {
    console.log('[App] showLoginScreen');
    console.log('[App] showLoginScreen trace');
    const loginScreen = document.getElementById('login-screen');
    const appEl = document.getElementById('app');
    if (loginScreen) loginScreen.hidden = false;
    if (appEl) appEl.hidden = true;
    I18n.translatePage(); // Ensure login screen is translated
  }

  // Pokaz aplikacje, ukryj ekran logowania
  function showAppScreen() {
    console.log('[App] showAppScreen');
    const loginScreen = document.getElementById('login-screen');
    const appEl = document.getElementById('app');
    console.log('[App] loginScreen:', !!loginScreen, 'appEl:', !!appEl);
    if (loginScreen) loginScreen.hidden = true;
    if (appEl) appEl.hidden = false;
    console.log('[App] loginScreen.hidden=', loginScreen?.hidden, 'appEl.hidden=', appEl?.hidden);
    I18n.translatePage(); // Ensure app screen is translated
  }

  // Aktualizacja statusu polaczenia WS
  function updateConnectionStatus(status) {
    const badge = document.getElementById('connection-status');
    if (!badge) return;

    if (status === 'connected') {
      badge.className = 'status-badge status-connected';
      badge.textContent = I18n.t('topbar.connected');
      badge.setAttribute('data-i18n', 'topbar.connected');
    } else {
      badge.className = 'status-badge status-disconnected';
      badge.textContent = I18n.t('topbar.disconnected');
      badge.setAttribute('data-i18n', 'topbar.disconnected');
    }
  }

  // Inicjalizacja kontenera toastow
  function initToastContainer() {
    if (document.querySelector('.toast-container')) return;
    const container = document.createElement('div');
    container.className = 'toast-container';
    document.body.appendChild(container);
    toastContainerRef = container;
  }

  // Wyswietlenie toasta z rate-limitem (max 5/s)
  function showToast(message, type) {
    type = type || 'info';
    const container = toastContainerRef || document.querySelector('.toast-container');
    if (!container) return;

    // Sprawdz rate-limit
    const now = Date.now();
    toastTimestamps = toastTimestamps.filter(t => now - t < TOAST_RATE_WINDOW);
    if (toastTimestamps.length >= TOAST_RATE_LIMIT) return;
    toastTimestamps.push(now);

    const toast = document.createElement('div');
    toast.className = `toast toast-${type}`;
    toast.textContent = message;
    container.appendChild(toast);

    // Auto-usun po 4 sekundach
    setTimeout(() => {
      toast.style.opacity = '0';
      toast.style.transform = 'translateX(40px)';
      toast.style.transition = 'all 0.3s ease';
      setTimeout(() => {
        if (toast.parentNode) toast.parentNode.removeChild(toast);
      }, 300);
    }, 4000);
  }

  return {
    init,
    showToast,
  };
})();

// Obsluga mobile menu (hamburger toggle)
function initMobileMenu() {
  const menuToggle = document.getElementById('mobile-menu-toggle');
  const sidebar = document.querySelector('.sidebar');
  const overlay = document.getElementById('sidebar-overlay');

  if (!menuToggle || !sidebar) return;

  menuToggle.addEventListener('click', () => {
    sidebar.classList.toggle('open');
    menuToggle.classList.toggle('active');
    if (overlay) overlay.classList.toggle('active');
  });

  // Zamknij po kliknieciu overlay
  if (overlay) {
    overlay.addEventListener('click', () => {
      sidebar.classList.remove('open');
      menuToggle.classList.remove('active');
      overlay.classList.remove('active');
    });
  }

  // Zamknij po wybraniu pozycji menu
  sidebar.addEventListener('click', (e) => {
    if (e.target.closest('[data-view]')) {
      sidebar.classList.remove('open');
      menuToggle.classList.remove('active');
      if (overlay) overlay.classList.remove('active');
    }
  });
}

// Uruchomienie po zaladowaniu DOM
document.addEventListener('DOMContentLoaded', () => {
  App.init();
  initMobileMenu();
});
