// =============================================================================
// Plik: app.js
// Opis: Entry point. Sprawdza JWT, jesli brak → login screen, w innym wypadku
//       montuje app shell (sidebar + topbar + content) i nawigacje.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { codecReady } from '/js/protocol/codec.js';
import { Router } from '/js/router.js';
import { icon } from '/js/icons.js';
import { byId, escapeHtml, toast } from '/js/utils.js';

import LoginScreen from '/js/modules/login.js';
import DashboardScreen from '/js/modules/dashboard.js';
import ModelsScreen from '/js/modules/models.js';
import ServicesScreen from '/js/modules/services.js';
import HubScreen from '/js/modules/hub.js';
import MeshScreen from '/js/modules/mesh.js';
import ClustersScreen from '/js/modules/clusters.js';
import FlowsScreen from '/js/modules/flows.js';
import ChatScreen from '/js/modules/chat.js';
import PromptsScreen from '/js/modules/prompts.js';
import RegistriesScreen from '/js/modules/registries.js';
import RulesScreen from '/js/modules/rules.js';
import ApiKeysScreen from '/js/modules/apikeys.js';
import UsersScreen from '/js/modules/users.js';
import SettingsScreen from '/js/modules/settings.js';
import AuditScreen from '/js/modules/audit.js';

const SIDEBAR_SECTIONS = [
  {
    title: 'Główne',
    items: [
      { id: 'dashboard', label: 'Dashboard', icon: 'dashboard' },
      { id: 'chat', label: 'Chat', icon: 'chat' },
    ],
  },
  {
    title: 'AI',
    items: [
      { id: 'models', label: 'Modele', icon: 'models' },
      { id: 'services', label: 'Serwisy', icon: 'services' },
      { id: 'hub', label: 'Hub silników', icon: 'hub' },
      { id: 'prompts', label: 'Prompty', icon: 'prompts' },
      { id: 'flows', label: 'Flows', icon: 'flows' },
    ],
  },
  {
    title: 'Mesh',
    items: [
      { id: 'mesh', label: 'Peers', icon: 'mesh' },
      { id: 'clusters', label: 'Klastry', icon: 'clusters' },
    ],
  },
  {
    title: 'Administracja',
    items: [
      { id: 'apikeys', label: 'Klucze API', icon: 'apikeys' },
      { id: 'users', label: 'Użytkownicy', icon: 'users' },
      { id: 'rules', label: 'Reguły', icon: 'rules' },
      { id: 'registries', label: 'Rejestry', icon: 'registries' },
      { id: 'settings', label: 'Ustawienia', icon: 'settings' },
      { id: 'audit', label: 'Audit log', icon: 'audit' },
    ],
  },
];

async function bootstrap() {
  await codecReady;

  if (!ApiBinary.hasJwt()) {
    renderLogin();
    return;
  }

  // Verify JWT against /ws/api by trying authMeRequest.
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
  LoginScreen.mount({
    onSuccess: () => renderApp(),
  });
}

async function renderApp() {
  const root = byId('app-root');
  const me = await ApiBinary.one('authMeRequest').catch(() => null);

  root.innerHTML = `
    <div class="app-shell">
      <aside class="sidebar">
        <div class="sidebar-brand">
          <div class="sidebar-brand-mark">T</div>
          <div class="sidebar-brand-name">TentaFlow</div>
        </div>
        ${SIDEBAR_SECTIONS.map((section) => `
          <div class="sidebar-section">
            <div class="sidebar-section-title">${escapeHtml(section.title)}</div>
            ${section.items.map((item) => `
              <a class="nav-item" data-view="${item.id}">
                ${icon(item.icon)}
                <span>${escapeHtml(item.label)}</span>
              </a>
            `).join('')}
          </div>
        `).join('')}
        <div class="sidebar-footer">
          <a class="nav-item" id="nav-logout" style="color: var(--color-text-muted);">
            ${icon('logout')}
            <span>Wyloguj</span>
          </a>
        </div>
      </aside>
      <header class="topbar">
        <h1 class="topbar-title" id="topbar-title">Dashboard</h1>
        <div class="topbar-actions">
          <span class="connection-pill" id="connection-pill">
            <span class="status-dot online"></span>
            <span id="connection-status">Połączono</span>
          </span>
          <span class="user-pill">
            <span class="user-avatar">${(me?.username ?? '?').charAt(0).toUpperCase()}</span>
            <span>${escapeHtml(me?.username ?? 'user')}</span>
            <span class="badge badge-accent">${escapeHtml(me?.role ?? 'user')}</span>
          </span>
        </div>
      </header>
      <main class="content" id="content"></main>
    </div>
  `;

  // Register screens.
  Router.register('dashboard', DashboardScreen);
  Router.register('chat', ChatScreen);
  Router.register('models', ModelsScreen);
  Router.register('services', ServicesScreen);
  Router.register('hub', HubScreen);
  Router.register('prompts', PromptsScreen);
  Router.register('flows', FlowsScreen);
  Router.register('mesh', MeshScreen);
  Router.register('clusters', ClustersScreen);
  Router.register('apikeys', ApiKeysScreen);
  Router.register('users', UsersScreen);
  Router.register('rules', RulesScreen);
  Router.register('registries', RegistriesScreen);
  Router.register('settings', SettingsScreen);
  Router.register('audit', AuditScreen);

  Router.init('dashboard');

  byId('nav-logout')?.addEventListener('click', (e) => {
    e.preventDefault();
    ApiBinary.clearSession();
    renderLogin();
  });

  // Heartbeat dla connection indicator.
  setInterval(async () => {
    try {
      const start = performance.now();
      await ApiBinary.one('metaHeartbeat', BigInt(Math.floor(Date.now() / 1000)));
      const rtt = Math.round(performance.now() - start);
      const status = byId('connection-status');
      if (status) status.textContent = `${rtt}ms`;
    } catch {
      const status = byId('connection-status');
      if (status) status.textContent = 'offline';
      const dot = document.querySelector('#connection-pill .status-dot');
      if (dot) dot.className = 'status-dot offline';
    }
  }, 5000);
}

window.addEventListener('error', (e) => {
  console.error('[app] uncaught:', e.error);
});

bootstrap().catch((err) => {
  console.error('[app] bootstrap failed', err);
  document.body.innerHTML = `<div style="padding: 2rem; color: #ef4444;">Bootstrap error: ${err.message}</div>`;
});
