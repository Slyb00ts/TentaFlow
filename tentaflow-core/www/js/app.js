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
// Expose router globally for modules ladowane dynamicznie (profile-report,
// profile-compare, profile-permissions, profiling-sessions) ktore robia
// `window.Router.navigate(...)` zamiast staleego importu — to celowe
// "weak coupling" zeby uniknac cyklicznych importow w drill-down widokach.
if (typeof window !== 'undefined') window.Router = Router;
import { byId, escapeHtml } from '/js/utils.js';
import { I18n, SUPPORTED_LANGS } from '/js/i18n.js';
import '/js/components/index.js';
import '/js/lib/block-zoom.js';
import * as ConnectionOverlay from '/js/modules/connection-overlay.js';
import * as UpdateOverlay from '/js/modules/update-overlay.js';
import * as SystemEvents from '/js/modules/system-events.js';
import { initTransport } from '/js/protocol/api-binary-shim.js';

import LoginScreen from '/js/modules/login.js';
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
import RulesScreen from '/js/modules/rules.js';
import UsersScreen from '/js/modules/users.js';
import AccessKeysScreen from '/js/modules/access-keys.js';
import SettingsScreen from '/js/modules/settings.js';
import AuditScreen from '/js/modules/audit.js';
import EventsScreen from '/js/modules/events.js';
import AddonsScreen from '/js/modules/addons.js';
import AddonAppScreen from '/js/modules/addon-app.js';
import MyAccountsScreen from '/js/modules/my-accounts.js';
import AppsHomeScreen from '/js/modules/apps-home.js';
import ProfileScreen from '/js/modules/profile.js';
import SettingsUserScreen from '/js/modules/settings-user.js';
import TranslateScreen from '/js/modules/translate.js';
import MeetingScreen from '/js/modules/meeting.js';
import MeetingLiveScreen from '/js/modules/meeting-live.js';
import ProfileReportView from '/js/modules/profile-report.js';
import ProfileCompareView from '/js/modules/profile-compare.js';
import ProfilePermissionsView from '/js/modules/profile-permissions.js';
import ProfilingSessionsScreen from '/js/modules/profiling-sessions-screen.js';
import LegalScreen from '/js/modules/legal/index.js';
import SchedulerScreen from '/js/modules/scheduler.js';
import AnalyticsScreen from '/js/modules/analytics.js';
import BenchmarkStudioScreen from '/js/modules/benchmark-studio.js';
import MlStudioScreen from '/js/modules/ml-studio.js';
import RobotsScreen from '/js/modules/robots.js';
import RolesCatalogScreen from '/js/modules/roles_catalog.js';
import SkillsScreen from '/js/modules/skills.js';
import AgentsScreen from '/js/modules/agents.js';
import ProjectStudioScreen from '/js/modules/project-studio.js';
import CodeStudioScreen from '/js/modules/code-studio.js';

// Adapter: profile-report eksponuje statyczne `render(container, params)`,
// podczas gdy Router oczekuje `show(params)`. Owijamy je w minimalny screen
// object zeby Router.navigate('profile-report', ...) zadzialalo.
const ProfileReportScreen = {
  title: 'Profile Report',
  async show(params = {}) {
    const main = document.getElementById('main');
    if (!main) return;
    await ProfileReportView.render(main, params);
  },
};

const ProfileCompareScreen = {
  title: 'Compare Profile Sessions',
  async show(params = {}) {
    const main = document.getElementById('main');
    if (!main) return;
    await ProfileCompareView.render(main, params);
  },
};

const ProfilePermissionsScreen = {
  title: 'Profile Permissions',
  async show() {
    const main = document.getElementById('main');
    if (!main) return;
    await ProfilePermissionsView.render(main);
  },
};

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
      { id: 'scheduler', labelKey: 'nav.scheduler', icon: 'clock' },
      { id: 'rules', labelKey: 'nav.rules', icon: 'rules' },
    ],
  },
  {
    headingKey: 'nav.section_ai_agents',
    icon: 'brain',
    items: [
      { id: 'agents', labelKey: 'nav.agents', icon: 'brain' },
      { id: 'skills', labelKey: 'nav.skills', icon: 'sparkle' },
      { id: 'robots', labelKey: 'nav.robots', icon: 'cpu' },
    ],
  },
  {
    headingKey: 'nav.section_management',
    icon: 'management',
    items: [
      { id: 'services', labelKey: 'nav.services', icon: 'services' },
      { id: 'catalog', labelKey: 'nav.catalog', icon: 'catalog' },
      { id: 'settings', labelKey: 'nav.settings', icon: 'settings' },
      { id: 'addons', labelKey: 'nav.addons', icon: 'puzzle' },
      { id: 'users', labelKey: 'nav.users', icon: 'users' },
      { id: 'access-keys', labelKey: 'nav.access_keys', icon: 'key' },
      { id: 'roles-catalog', labelKey: 'nav.roles_catalog', icon: 'key' },
      { id: 'audit', labelKey: 'nav.audit', icon: 'audit' },
      { id: 'events', labelKey: 'nav.events', icon: 'clock-glance', userVisible: true },
      { id: 'analytics', labelKey: 'nav.analytics', icon: 'trend' },
      { id: 'legal', labelKey: 'nav.legal', icon: 'audit' },
      { id: 'profiling-sessions', labelKey: 'nav.profiling_sessions', icon: 'trend' },
    ],
  },
];

// Management entries a plain user may reach. `userVisible` is the per-item
// counterpart of `requiresPowerUser`: the screen behind such an item narrows
// itself to what the caller may see (Zdarzenia answers `scoped_to_self` when the
// caller holds `events.read` but not `events.read_all`), so hiding the entry
// would only hide the user's own data from them.
function userVisibleAdminSections() {
  return ADMIN_NAV
    .map((section) => ({ ...section, items: section.items.filter((it) => it.userVisible) }))
    .filter((section) => section.items.length > 0);
}

// Apps section shared by every role — always the first block of the sidebar.
// `requiresPowerUser` items are dropped at render time for plain users, mirroring
// the TILES gate in apps-home.js and the backend PowerUser policy.
// Legacy hardcoded nav items — SHRINKS as apps move onto the app-platform
// (plan-01 P2): a migrated app arrives via appsListRequest (sidebar injection)
// and must be removed here. Benchmark Studio already migrated.
const APPS_NAV = {
  headingKey: 'nav.section_apps',
  icon: 'apps',
  items: [
    { id: 'apps-home', labelKey: 'nav.apps_home', icon: 'apps' },
    { id: 'chat', labelKey: 'nav.chat', icon: 'chat' },
    { id: 'meeting', labelKey: 'nav.meeting', icon: 'meeting' },
    { id: 'translate', labelKey: 'nav.translate', icon: 'globe' },
  ],
};

// Menu user per mockup #2.
const USER_NAV = [
  APPS_NAV,
  {
    headingKey: 'nav.section_account',
    icon: 'user',
    items: [
      { id: 'profile', labelKey: 'nav.profile', icon: 'user' },
      { id: 'my-accounts', labelKey: 'nav.my_accounts', icon: 'share' },
    ],
  },
];

async function bootstrap() {
  await Promise.all([codecReady, I18n.init()]);

  // Overlay init przed otwarciem WS — zeby wszystkie lifecycle events byly
  // przechwycone od pierwszej chwili.
  ConnectionOverlay.init();
  UpdateOverlay.init();

  // Otworz WS natychmiast (anonymous jesli brak JWT). Serwer akceptuje i
  // pozwala tylko na authLogin + schema + heartbeat przed zalogowaniem.
  initTransport().catch((e) => console.warn('[app] initTransport:', e?.message));
  SystemEvents.init();

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

  // Protocol handler — gdy aplikacja jest zainstalowana jako PWA i user
  // klika link tentaflow-pair://<hex>?pin=<pin>, przegladarka otwiera nas
  // z /?pair=<encoded-uri>. Parsujemy query i otwieramy pair flow z
  // auto-submitem. Sesja uzytkownika musi byc zalogowana.
  handlePairDeepLink();

  // Service Worker registration — precache calego frontu + offline shell.
  // Wymaga secure context (https albo localhost). Chrome odrzuca SW na
  // self-signed HTTPS z cert error — to jest zlapane w .catch (SW to tylko
  // optymalizacja; wykrywanie nieaktualnego frontu i tak dziala przez handshake
  // WS niezaleznie od SW). Na zaufanym certcie/localhost SW sie zarejestruje.
  if ('serviceWorker' in navigator && window.isSecureContext) {
    // updateViaCache:'none' — sw.js ORAZ jego importScripts (sw-version.js) sa
    // przy sprawdzaniu update'u pobierane z sieci, nie z HTTP cache, wiec zmiana
    // build-hasha jest zawsze wykryta.
    navigator.serviceWorker.register('/sw.js', { updateViaCache: 'none' }).catch((e) => {
      console.debug('[app] SW register failed:', e?.message);
    });
  }
}

async function handlePairDeepLink() {
  const params = new URLSearchParams(window.location.search);
  const pairRaw = params.get('pair');
  if (!pairRaw) return;
  if (!ApiBinary.hasJwt()) return; // musi byc zalogowany
  try {
    const qrScanner = await import('/js/modules/qr-scanner.js');
    const parsed = qrScanner.parsePairUri(decodeURIComponent(pairRaw));
    if (!parsed) return;
    // Wywolaj pairing start z kompletem hintow transportowych z QR invite.
    const resp = await ApiBinary.action('meshPairingStartRequest', {
      remoteAddress: parsed.hex,
      pin: parsed.pin || '',
      ...(parsed.publicKey ? { remotePublicKey: parsed.publicKey } : {}),
      ...(parsed.addresses?.length ? { remoteAddresses: parsed.addresses } : {}),
      ...(parsed.relayUrl ? { remoteRelayUrl: parsed.relayUrl } : {}),
      ...(parsed.host ? { remoteHostname: parsed.host } : {}),
    });
    if (resp?.completed) {
      console.info('[pair-deep-link] pairing completed:', parsed.hex);
    } else if (resp?.pin) {
      console.warn('[pair-deep-link] pairing did not auto-complete');
    }
    // Wyczysc query string zeby przy F5 nie robilo sie znowu.
    const url = new URL(window.location.href);
    url.searchParams.delete('pair');
    window.history.replaceState({}, '', url.pathname + url.hash);
  } catch (e) {
    console.warn('[pair-deep-link]', e?.message);
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
  // Power User to rola posrednia miedzy `user` a `admin` (zob. users.js: role
  // moze byc 'admin' | 'power_user' | 'user'). Admin jest nadzbiorem Power Usera.
  const isPowerUser = isAdmin || role === 'power_user';
  const initials = (me?.username ?? '?').slice(0, 2).toUpperCase();

  function paint() {
    // Admin is a superset of user: apps first, then admin sections, account last.
    const nav = (isAdmin
      ? [APPS_NAV, ...ADMIN_NAV, ...USER_NAV.slice(1)]
      : [APPS_NAV, ...userVisibleAdminSections(), ...USER_NAV.slice(1)])
      .map((section) => ({
        ...section,
        items: section.items.filter((it) => !it.requiresPowerUser || isPowerUser),
      }));
    const userClass = isAdmin ? 'admin' : isPowerUser ? 'power' : 'user';
    const roleLabel = I18n.t(
      isAdmin ? 'role.administrator' : isPowerUser ? 'users.role_power' : 'role.user',
    );
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

    // Async: dorzuc addon application tiles do sidebar pod sekcja Apps.
    // Bez tego addon z [application] manifestu pojawial sie tylko w
    // `apps-home` grid, ale nie w lewym menu.
    injectAddonAppsIntoSidebar();
  }

  async function injectAddonAppsIntoSidebar() {
    let apps;
    try {
      apps = await ApiBinary.list('appsListRequest', { arrayKey: 'apps' });
    } catch (e) {
      console.warn('[app] appsListRequest fail:', e?.message ?? e);
      return;
    }
    if (!Array.isArray(apps) || apps.length === 0) return;

    // Stabilny porzadek na dole sekcji Apps: sort_order ASC, potem title ASC.
    const sorted = apps.slice().sort((a, b) => {
      const sa = Number(a.sortOrder ?? a.sort_order ?? 0);
      const sb = Number(b.sortOrder ?? b.sort_order ?? 0);
      if (sa !== sb) return sa - sb;
      const ta = String(a.title ?? a.addonId ?? a.addon_id ?? '').toLowerCase();
      const tb = String(b.title ?? b.addonId ?? b.addon_id ?? '').toLowerCase();
      return ta.localeCompare(tb);
    });

    // Znajdz sekcje Apps po headingu (i18n key nav.section_apps).
    const appsHeading = I18n.t('nav.section_apps');
    const sections = document.querySelectorAll('.sidebar .nav-section');
    let appsSection = null;
    sections.forEach((s) => {
      const h = s.querySelector('.heading');
      if (h && h.textContent.trim().endsWith(appsHeading)) {
        appsSection = s;
      }
    });
    if (!appsSection) return;

    for (const app of sorted) {
      const addonId = String(app.addonId ?? app.addon_id ?? '');
      const kind = app.kind === 'native' ? 'native' : 'wasm';
      const target = String(app.target ?? app.entryPanel ?? app.entry_panel ?? '');
      const title = String(
        (app.titleKey && I18n.t(app.titleKey)) || app.title || addonId,
      );
      const enabled = app.enabled !== false;
      const iconId = resolveAddonIcon(app.icon);
      if (!addonId || !target) continue;

      const item = document.createElement('div');
      item.className = 'nav-item addon-app-nav-item';
      item.dataset.addonId = addonId;
      item.dataset.kind = kind;
      item.dataset.target = target;
      item.dataset.view = kind === 'native' ? target : `addon-app:${addonId}`;
      const disabledBadge = enabled
        ? ''
        : `<span class="badge soon">${escapeHtml(I18n.t('addon.disabled') || 'disabled')}</span>`;
      item.innerHTML =
        `<svg class="icon"><use href="#i-${iconId}"/></svg>` +
        `<span>${escapeHtml(title)}</span>${disabledBadge}`;
      if (!enabled) {
        item.classList.add('disabled');
        item.setAttribute('aria-disabled', 'true');
      }
      item.addEventListener('click', (ev) => {
        ev.preventDefault();
        if (!enabled) return;
        document.querySelectorAll('.sidebar .nav-item.active').forEach((a) => a.classList.remove('active'));
        item.classList.add('active');
        if (kind === 'native') {
          Router.navigate(target);
        } else {
          Router.navigate('addon-app', { addonId, panelId: target });
        }
        closeDrawer();
      });
      appsSection.appendChild(item);
    }
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
  Router.register('catalog', CatalogScreen);
  Router.register('prompts', PromptsScreen);
  Router.register('flows', FlowsScreen);
  Router.register('flow-builder', FlowBuilderScreen);
  Router.register('scheduler', SchedulerScreen);
  Router.register('analytics', AnalyticsScreen);
  Router.register('benchmark-studio', BenchmarkStudioScreen);
  Router.register('ml-studio', MlStudioScreen);
  Router.register('robots', RobotsScreen);
  Router.register('skills', SkillsScreen);
  Router.register('agents', AgentsScreen);
  Router.register('code-studio', CodeStudioScreen);
  Router.register('projekty', ProjectStudioScreen);
  Router.register('mesh', MeshScreen);
  Router.register('clusters', ClustersScreen);
  Router.register('users', UsersScreen);
  Router.register('access-keys', AccessKeysScreen);
  Router.register('roles-catalog', RolesCatalogScreen);
  Router.register('rules', RulesScreen);
  Router.register('settings', SettingsScreen);
  Router.register('audit', AuditScreen);
  Router.register('events', EventsScreen);
  Router.register('legal', LegalScreen);
  Router.register('addons', AddonsScreen);
  // Drill-down: Router.navigate('addon-app', { addonId, panelId }) z apps-home.
  Router.register('addon-app', AddonAppScreen);
  Router.register('my-accounts', MyAccountsScreen);
  Router.register('apps-home', AppsHomeScreen);
  Router.register('profile', ProfileScreen);
  Router.register('settings-user', SettingsUserScreen);
  Router.register('meeting', MeetingScreen);
  Router.register('meeting-live', MeetingLiveScreen);
  Router.register('translate',      TranslateScreen);
  Router.register('profile-report', ProfileReportScreen);
  Router.register('profile-compare', ProfileCompareScreen);
  Router.register('profile-permissions', ProfilePermissionsScreen);
  Router.register('profiling-sessions', ProfilingSessionsScreen);

  paint();

  // Po zmianie jezyka odswiezamy shell + biezacy widok zeby wszystkie label'e zostaly przelozone.
  I18n.subscribe(async () => {
    const current = Router.current();
    paint();
    const initial = document.querySelector(`[data-view="${current ?? 'apps-home'}"]`);
    if (initial) initial.classList.add('active');
    await Router.navigate(current ?? 'apps-home');
  });

  Router.init('apps-home');
  const initial = document.querySelector('[data-view="apps-home"]');
  if (initial) initial.classList.add('active');

  // Liczniki w sidebar menu — pobieramy po zamontowaniu shellu i odswiezamy
  // co 30s. Gdy jakis endpoint nie odpowie, silently pomijamy (dedupe toastow
  // w utils.js i tak uchroni przed spam'em bledow).
  refreshNavCounts();
  setInterval(refreshNavCounts, 30000);
  // Ekrany, ktore same dodaja/usuwaja zliczane obiekty, nie moga czekac do 30 s
  // na kolejny tick — inaczej badge pokazuje nieistniejacy juz serwis.
  window.addEventListener('tf:nav-counts-stale', () => { refreshNavCounts(); });
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

// Whitelista ikon w sprite (zob. www/index.html <symbol id="i-...">). Addon
// moze podac dowolny string w manifescie [application.icon]; jezeli nie pasuje
// — uzywamy generycznego "apps".
const ADDON_ICON_WHITELIST = new Set([
  'alert', 'apps', 'arrow', 'arrow-left', 'arrow-out', 'audit', 'ban',
  'bar-chart', 'bolt', 'bot', 'brain', 'branch', 'catalog', 'chart-line',
  'chat', 'check', 'check-circle', 'chevron-down',
  'chevron-left', 'chevron-right', 'chip', 'clock', 'clock-glance', 'close',
  'cloud', 'cluster', 'code', 'collapse', 'copy', 'core', 'cpu', 'cylinder',
  'dashboard', 'database', 'desktop', 'docker', 'download', 'edit',
  'external-link', 'eye', 'file', 'file-text', 'filter', 'flask', 'flow',
  'folder', 'git', 'globe',
  'globe-grid', 'gpu', 'grid-rows', 'grip', 'home', 'home-simple', 'host',
  'iface-lan', 'iface-loop', 'iface-tb', 'iface-virt', 'iface-vpn',
  'iface-wifi', 'image', 'info', 'key', 'layers', 'line-chart', 'list', 'lock',
  'logout',
  'management', 'max', 'meeting', 'message', 'mic', 'min', 'model', 'models',
  'network', 'network-svg', 'os', 'paperclip', 'pause', 'pi', 'pin', 'play',
  'plus', 'prompt', 'puzzle', 'question', 'rag-db', 'ram', 'record',
  'record-dot', 'refresh', 'registry', 'rotate', 'rules', 'save', 'search',
  'send',
  'services', 'settings', 'share', 'shield', 'sparkle', 'speaker',
  'speaker-alt', 'star', 'stop', 'terminal', 'transform', 'trash', 'trend',
  'unlock',
  'user', 'users', 'volume', 'workflow-app', 'x', 'zap',
]);

function resolveAddonIcon(raw) {
  const trimmed = String(raw ?? '').trim();
  const stripped = trimmed.startsWith('i-') ? trimmed.slice(2) : trimmed;
  if (stripped && ADDON_ICON_WHITELIST.has(stripped)) return stripped;
  return 'apps';
}

window.addEventListener('error', (e) => {
  // Benign browser notice fired when observers trigger layout in the same
  // frame (tf-tabs / chart resize observers) — not an application error.
  if (typeof e.message === 'string' && e.message.startsWith('ResizeObserver loop')) return;
  console.error('[app] uncaught:', e.error ?? e.message);
});

bootstrap().catch((err) => {
  console.error('[app] bootstrap failed', err);
  document.body.innerHTML = `<div style="padding: 2rem; color: #ef4444;">Bootstrap error: ${escapeHtml(err.message)}</div>`;
});
