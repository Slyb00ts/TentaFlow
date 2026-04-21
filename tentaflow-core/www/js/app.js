// =============================================================================
// Plik: app.js
// Opis: Punkt wejscia aplikacji. Inicjalizuje codec WASM oraz tlumaczenia,
//       weryfikuje JWT, montuje shell aplikacji (sidebar 260 px + main) z
//       hierarchicznym menu zaleznym od roli (admin/user) oraz dolnym
//       przelacznikiem jezyka.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { codecReady } from '/js/protocol/codec.js';
import { Router } from '/js/router.js';
import { byId, escapeHtml } from '/js/utils.js';
import { I18n, SUPPORTED_LANGS } from '/js/i18n.js';
import '/js/components/index.js';
import '/js/lib/block-zoom.js';

import LoginScreen from '/js/modules/login.js';
import FaceBackground from '/js/modules/faceBackground.js';
import DashboardScreen from '/js/modules/dashboard.js';
import ServicesScreen from '/js/modules/services.js';
import HubScreen from '/js/modules/hub.js';
import CatalogScreen from '/js/modules/catalog.js';
import MeshScreen from '/js/modules/mesh.js';
import ClustersScreen from '/js/modules/clusters.js';
import FlowsScreen from '/js/modules/flows.js';
import FlowBuilderScreen from '/js/modules/flows-builder.js';
import ChatScreen from '/js/modules/chat.js';
import PromptsScreen from '/js/modules/prompts.js';
import RegistriesScreen from '/js/modules/registries.js';
import RulesScreen from '/js/modules/rules.js';
import ApiKeysScreen from '/js/modules/apikeys.js';
import UsersScreen from '/js/modules/users.js';
import SettingsScreen from '/js/modules/settings.js';
import AuditScreen from '/js/modules/audit.js';
import AddonsScreen from '/js/modules/addons.js';
import MyAccountsScreen from '/js/modules/my-accounts.js';
import AppsHomeScreen from '/js/modules/apps-home.js';
import ProfileScreen from '/js/modules/profile.js';
import SettingsUserScreen from '/js/modules/settings-user.js';
import FlowsUserScreen from '/js/modules/flows-user.js';
import PromptsUserScreen from '/js/modules/prompts-user.js';
import TranslateScreen from '/js/modules/translate.js';
import NotesScreen from '/js/modules/notes.js';
import { makeComingSoonScreen } from '/js/modules/coming-soon.js';

// Helper: SVG <use> reference do inline sprite.
function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

// Pelny menu admin per mockup #1 — labele zamiast tekstu trzymane jako klucze i18n.
const ADMIN_NAV = [
  {
    headingKey: 'nav.section_general',
    icon: 'settings',
    items: [
      { id: 'dashboard', labelKey: 'nav.dashboard', icon: 'dashboard' },
      { id: 'services', labelKey: 'nav.services', icon: 'services' },
      { id: 'apikeys', labelKey: 'nav.apikeys', icon: 'key' },
      { id: 'settings', labelKey: 'nav.settings', icon: 'settings' },
    ],
  },
  {
    headingKey: 'nav.section_core',
    icon: 'core',
    items: [
      { id: 'mesh', labelKey: 'nav.mesh', icon: 'network' },
      { id: 'clusters', labelKey: 'nav.clusters', icon: 'cluster' },
      { id: 'prompts', labelKey: 'nav.prompts', icon: 'prompt' },
    ],
  },
  {
    headingKey: 'nav.section_workflows',
    icon: 'flow',
    items: [
      { id: 'flows', labelKey: 'nav.flows', icon: 'flow' },
      { id: 'playground', labelKey: 'nav.playground', icon: 'play' },
      { id: 'rules', labelKey: 'nav.rules', icon: 'rules' },
      { id: 'registries', labelKey: 'nav.registries', icon: 'registry' },
    ],
  },
  {
    headingKey: 'nav.section_integrations',
    icon: 'puzzle',
    items: [
      { id: 'catalog', labelKey: 'nav.catalog', icon: 'catalog' },
    ],
  },
  {
    headingKey: 'nav.section_management',
    icon: 'management',
    items: [
      { id: 'addons', labelKey: 'nav.addons', icon: 'puzzle' },
      { id: 'users', labelKey: 'nav.users', icon: 'users' },
      { id: 'audit', labelKey: 'nav.audit', icon: 'audit' },
    ],
  },
];

// Menu user per mockup #2.
const USER_NAV = [
  {
    headingKey: 'nav.section_apps',
    icon: 'apps',
    items: [
      { id: 'apps-home', labelKey: 'nav.apps_home', icon: 'apps' },
      { id: 'chat', labelKey: 'nav.chat', icon: 'chat' },
      { id: 'images', labelKey: 'nav.images', icon: 'image', badge: 'soon' },
      { id: 'notes', labelKey: 'nav.notes', icon: 'mic' },
      { id: 'meeting', labelKey: 'nav.meeting', icon: 'meeting', badge: 'soon' },
      { id: 'flows-user', labelKey: 'nav.flows_user', icon: 'workflow-app' },
      { id: 'prompts-user', labelKey: 'nav.prompts_user', icon: 'star' },
      { id: 'tts', labelKey: 'nav.tts', icon: 'speaker', badge: 'soon' },
      { id: 'translate', labelKey: 'nav.translate', icon: 'globe' },
      { id: 'search-app', labelKey: 'nav.search_app', icon: 'search', badge: 'soon' },
    ],
  },
  {
    headingKey: 'nav.section_network',
    icon: 'network',
    items: [
      { id: 'mesh-user', labelKey: 'nav.mesh_user', icon: 'network', badge: 'soon' },
      { id: 'tailscale-user', labelKey: 'nav.tailscale_user', icon: 'zap', badge: 'soon' },
    ],
  },
  {
    headingKey: 'nav.section_account',
    icon: 'user',
    items: [
      { id: 'profile', labelKey: 'nav.profile', icon: 'user' },
      { id: 'my-accounts', labelKey: 'nav.my_accounts', icon: 'share' },
      { id: 'settings-user', labelKey: 'nav.settings_user', icon: 'settings' },
    ],
  },
];

async function bootstrap() {
  await Promise.all([codecReady, I18n.init()]);

  if (!ApiBinary.hasJwt()) {
    renderLogin();
    return;
  }

  try {
    await ApiBinary.one('authMeRequest');
    renderApp();
  } catch (err) {
    console.warn('[app] JWT invalid or stale, returning to login:', err.message);
    ApiBinary.clearSession();
    renderLogin();
  }
}

function renderLogin() {
  const root = byId('app-root');
  root.innerHTML = LoginScreen.render();
  LoginScreen.mount({ onSuccess: () => renderApp() });
  I18n.applyDataI18n();
}

async function renderApp() {
  // Face-bg chowa się sam po zakończeniu `transitionOut`. Dla przypadku
  // świeżego JWT (bez ekranu logowania) `hide()` i tak nie zostaje wywołany,
  // bo kontener `.face-bg` nie istnieje — `hide()` robi wtedy no-op.
  const root = byId('app-root');
  const me = await ApiBinary.one('authMeRequest').catch(() => null);
  const role = (me?.role ?? 'user').toLowerCase();
  const isAdmin = role === 'admin';
  const initials = (me?.username ?? '?').slice(0, 2).toUpperCase();

  function paint() {
    // Admin sees their admin nav plus the user-facing apps appended — admin is a superset of user.
    const nav = isAdmin ? [...ADMIN_NAV, ...USER_NAV] : USER_NAV;
    const userClass = isAdmin ? 'admin' : 'user';
    const roleLabel = I18n.t(isAdmin ? 'role.administrator' : 'role.user');
    const logoutLabel = I18n.t('nav.logout');

    root.innerHTML = `
      <div class="app">
        <header class="mobile-header" id="mobile-header">
          <button class="mobile-menu-btn" id="mobile-menu-btn" aria-label="Menu">
            <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="3" y1="6" x2="21" y2="6"/><line x1="3" y1="12" x2="21" y2="12"/><line x1="3" y1="18" x2="21" y2="18"/></svg>
          </button>
          <div class="mobile-header-logo">
            <img src="/tentaflow.png" alt="" width="24">
            <span>TentaFlow</span>
          </div>
        </header>
        <div class="sidebar-backdrop" id="sidebar-backdrop"></div>
        <aside class="sidebar" id="app-sidebar">
          <div class="logo">
            <img class="octo" src="/tentaflow.png" alt="">
            <span class="name">TentaFlow</span>
          </div>
          <div class="sidebar-nav">
            ${nav.map((section) => `
              <div class="nav-section">
                <div class="heading">${sprite(section.icon)}${escapeHtml(I18n.t(section.headingKey))}</div>
                ${section.items.map((it) => `
                  <div class="nav-item" data-view="${it.id}">
                    ${sprite(it.icon)}
                    <span>${escapeHtml(I18n.t(it.labelKey))}</span>
                    <span class="nav-count" data-count-for="${it.id}" hidden></span>
                    ${it.badge ? `<span class="badge ${it.badge === 'soon' ? 'soon' : ''}">${escapeHtml(it.badge)}</span>` : ''}
                  </div>
                `).join('')}
              </div>
            `).join('')}
          </div>
          <div class="footer">
            <div class="lang-switcher" id="lang-switcher">
              <select class="lang-select" id="lang-select" title="${escapeHtml(I18n.t('lang.label'))}">
                ${SUPPORTED_LANGS.map((l) => `
                  <option value="${l.code}" ${l.code === I18n.getLanguage() ? 'selected' : ''}>${l.flag} ${escapeHtml(l.label)}</option>
                `).join('')}
              </select>
            </div>
            <div class="user-chip ${userClass}">
              <div class="avatar">${escapeHtml(initials)}</div>
              <div class="info">
                <div class="name-t">${escapeHtml(me?.username ?? 'unknown')}</div>
                <div class="role">${escapeHtml(roleLabel)}</div>
              </div>
            </div>
            <div class="nav-item logout" id="nav-logout">${sprite('logout')}<span>${escapeHtml(logoutLabel)}</span></div>
          </div>
        </aside>
        <main class="main" id="main"></main>
      </div>
    `;

    setupDrawer();

    document.querySelectorAll('.sidebar .nav-item[data-view]').forEach((el) => {
      el.addEventListener('click', (e) => {
        e.preventDefault();
        const view = el.dataset.view;
        document.querySelectorAll('.sidebar .nav-item.active').forEach((a) => a.classList.remove('active'));
        el.classList.add('active');
        Router.navigate(view);
        // Mobile: zamknij drawer po wyborze
        closeDrawer();
      });
    });

    byId('nav-logout')?.addEventListener('click', (e) => {
      e.preventDefault();
      ApiBinary.clearSession();
      renderLogin();
    });

    byId('lang-select')?.addEventListener('change', async (e) => {
      await I18n.setLanguage(e.target.value);
    });
  }

  function openDrawer() {
    document.body.classList.add('drawer-open');
  }
  function closeDrawer() {
    document.body.classList.remove('drawer-open');
  }
  function setupDrawer() {
    byId('mobile-menu-btn')?.addEventListener('click', () => {
      if (document.body.classList.contains('drawer-open')) closeDrawer();
      else openDrawer();
    });
    byId('sidebar-backdrop')?.addEventListener('click', closeDrawer);

    // Swipe from edge — otwarcie
    let touchStartX = null;
    document.addEventListener('touchstart', (e) => {
      if (e.touches[0].clientX < 20 && !document.body.classList.contains('drawer-open')) {
        touchStartX = e.touches[0].clientX;
      }
    }, { passive: true });
    document.addEventListener('touchmove', (e) => {
      if (touchStartX != null) {
        const dx = e.touches[0].clientX - touchStartX;
        if (dx > 60) {
          openDrawer();
          touchStartX = null;
        }
      }
    }, { passive: true });
    document.addEventListener('touchend', () => { touchStartX = null; }, { passive: true });

    // Swipe-left na otwartym drawerze — zamkniecie
    let drawerTouchX = null;
    const sidebar = byId('app-sidebar');
    sidebar?.addEventListener('touchstart', (e) => {
      if (document.body.classList.contains('drawer-open')) {
        drawerTouchX = e.touches[0].clientX;
      }
    }, { passive: true });
    sidebar?.addEventListener('touchmove', (e) => {
      if (drawerTouchX != null) {
        const dx = e.touches[0].clientX - drawerTouchX;
        if (dx < -60) {
          closeDrawer();
          drawerTouchX = null;
        }
      }
    }, { passive: true });
    sidebar?.addEventListener('touchend', () => { drawerTouchX = null; }, { passive: true });
  }

  Router.register('dashboard', DashboardScreen);
  Router.register('chat', ChatScreen);
  Router.register('services', ServicesScreen);
  Router.register('hub', HubScreen);
  // `catalog` nie ma w menu — serwisy z niego korzystają przy "Nowy serwis".
  Router.register('catalog', CatalogScreen);
  Router.register('prompts', PromptsScreen);
  Router.register('flows', FlowsScreen);
  Router.register('flow-builder', FlowBuilderScreen);
  Router.register('mesh', MeshScreen);
  Router.register('clusters', ClustersScreen);
  Router.register('apikeys', ApiKeysScreen);
  Router.register('users', UsersScreen);
  Router.register('rules', RulesScreen);
  Router.register('registries', RegistriesScreen);
  Router.register('settings', SettingsScreen);
  Router.register('audit', AuditScreen);
  Router.register('addons', AddonsScreen);
  Router.register('my-accounts', MyAccountsScreen);
  Router.register('apps-home', AppsHomeScreen);
  Router.register('profile', ProfileScreen);
  Router.register('settings-user', SettingsUserScreen);
  Router.register('flows-user', FlowsUserScreen);
  Router.register('prompts-user', PromptsUserScreen);
  Router.register('notes', NotesScreen);
  // Apps whose binary handlers are not yet wired — honest placeholder, not a stub feature.
  Router.register('images',         makeComingSoonScreen('images',    'image'));
  Router.register('meeting',        makeComingSoonScreen('meeting',   'meeting'));
  Router.register('tts',            makeComingSoonScreen('tts',       'speaker'));
  Router.register('translate',      TranslateScreen);
  Router.register('search-app',     makeComingSoonScreen('search',    'search'));
  Router.register('mesh-user',      makeComingSoonScreen('mesh_user', 'network'));
  Router.register('tailscale-user', makeComingSoonScreen('tailscale', 'zap'));

  paint();

  // Po zmianie jezyka odswiezamy shell + biezacy widok zeby wszystkie label'e zostaly przelozone.
  I18n.subscribe(async () => {
    const current = Router.current();
    paint();
    const initial = document.querySelector(`[data-view="${current ?? (isAdmin ? 'dashboard' : 'apps-home')}"]`);
    if (initial) initial.classList.add('active');
    await Router.navigate(current ?? (isAdmin ? 'dashboard' : 'apps-home'));
  });

  Router.init(isAdmin ? 'dashboard' : 'apps-home');
  const initial = document.querySelector(`[data-view="${isAdmin ? 'dashboard' : 'apps-home'}"]`);
  if (initial) initial.classList.add('active');

  // Liczniki w sidebar menu — pobieramy po zamontowaniu shellu i odswiezamy
  // co 30s. Gdy jakis endpoint nie odpowie, silently pomijamy (dedupe toastow
  // w utils.js i tak uchroni przed spam'em bledow).
  refreshNavCounts();
  setInterval(refreshNavCounts, 30000);
}

async function refreshNavCounts() {
  const setCount = (id, n) => {
    const el = document.querySelector(`.nav-count[data-count-for="${id}"]`);
    if (!el) return;
    if (typeof n === 'number' && n > 0) {
      el.textContent = String(n);
      el.hidden = false;
    } else {
      el.hidden = true;
      el.textContent = '';
    }
  };
  const len = (v) => Array.isArray(v) ? v.length : (v?.length ?? 0);
  // Wszystkie 5 zapytan przez binary WS — zero REST w refreshNavCounts.
  // Handler UsersListRequest wymaga policy Admin: dla zwyklych userow
  // serwer odpowie bledem i catch zwroci null (badge nie pokaze sie).
  const [svc, mesh, clusters, addons, users] = await Promise.all([
    ApiBinary.list('serviceListRequest').catch(() => null),
    ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).catch(() => null),
    ApiBinary.list('clusterListRequest', { arrayKey: 'clusters' }).catch(() => null),
    ApiBinary.list('addonsListRequest', { arrayKey: 'addons' }).catch(() => null),
    ApiBinary.list('usersListRequest', { arrayKey: 'users' }).catch(() => null),
  ]);
  if (svc !== null) setCount('services', len(svc));
  if (mesh !== null) setCount('mesh', len(mesh));
  if (clusters !== null) setCount('clusters', len(clusters));
  if (addons !== null) setCount('addons', len(addons));
  if (users !== null) setCount('users', len(users));
}

window.addEventListener('error', (e) => {
  console.error('[app] uncaught:', e.error);
});

bootstrap().catch((err) => {
  console.error('[app] bootstrap failed', err);
  document.body.innerHTML = `<div style="padding: 2rem; color: #ef4444;">Bootstrap error: ${err.message}</div>`;
});
