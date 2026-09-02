// =============================================================================
// File: modules/apps-home.js — Home: greeting banner + tiled apps grid.
// Rendered as the start screen for every role. Each tile navigates via Router.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { Router } from '/js/router.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeHtml } from '/js/utils.js';

// Legacy hardcoded tiles (kept in sync with app.js APPS_NAV). This list
// SHRINKS as apps move onto the app-platform (plan-01 P2) — a migrated app is
// served by appsListRequest instead and must be removed here, never listed
// twice. Benchmark Studio, ML Studio, Projekty, Code Studio and Meeting Bot
// already migrated.
// `requiresPowerUser` tiles are rendered only for Power User / Admin — the tile
// is filtered out before it ever reaches the DOM, mirroring the backend policy.
const TILES = [
  { id: 'chat',         route: 'chat',         icon: 'chat' },
  { id: 'translate',    route: 'translate',    icon: 'globe' },
];

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function renderTile(t) {
  const name = escapeHtml(I18n.t(`apps.${t.id}.name`));
  const desc = escapeHtml(I18n.t(`apps.${t.id}.desc`));
  return `
    <div class="app-tile" data-route="${escapeHtml(t.route)}">
      <div class="app-icon">${sprite(t.icon)}</div>
      <div class="app-name">${name}</div>
      <div class="app-desc">${desc}</div>
    </div>`;
}

// Whitelista ikon — synchronizowana z ADDON_ICON_WHITELIST w app.js.
// Nieznana lub nieprawidlowa ikona => 'apps'. Walidacja chroni przed XSS:
// raw input z manifestu addona NIE moze trafic do sprite() niesprawdzona.
const ICON_WHITELIST = new Set([
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

function resolveIcon(raw) {
  const t = String(raw ?? '').trim();
  const id = t.startsWith('i-') ? t.slice(2) : t;
  if (id && ICON_WHITELIST.has(id)) return id;
  return 'apps';
}

// Dynamic tile from the unified server list (AppEntryWire). kind decides the
// click: native -> Router screen (`target` = route id), wasm -> the addon-app
// panel renderer (`target` = entry panel id). Native apps may carry i18n keys.
function renderAddonTile(app) {
  const addonId = app.addonId ?? app.addon_id ?? '';
  const kind = app.kind === 'native' ? 'native' : 'wasm';
  const target = app.target ?? app.entryPanel ?? app.entry_panel ?? '';
  const title = escapeHtml(
    (app.titleKey && I18n.t(app.titleKey)) || app.title || addonId,
  );
  const desc = escapeHtml(
    (app.descriptionKey && I18n.t(app.descriptionKey)) || app.description || addonId,
  );
  const iconId = resolveIcon(app.icon);
  const enabled = app.enabled !== false;
  const disabledBadge = enabled
    ? ''
    : `<span class="badge-soon">${escapeHtml(I18n.t('addon.disabled') || 'disabled')}</span>`;
  const cls = `app-tile addon-app-tile${enabled ? '' : ' coming-soon'}`;
  return `
    <div class="${cls}"
         data-addon-id="${escapeHtml(addonId)}"
         data-kind="${kind}"
         data-target="${escapeHtml(target)}"
         data-enabled="${enabled ? '1' : '0'}">
      ${disabledBadge}
      <div class="app-icon">${sprite(iconId)}</div>
      <div class="app-name">${title}</div>
      <div class="app-desc">${desc}</div>
    </div>`;
}

function sortAddonApps(apps) {
  return apps.slice().sort((a, b) => {
    const sa = Number(a.sortOrder ?? a.sort_order ?? 0);
    const sb = Number(b.sortOrder ?? b.sort_order ?? 0);
    if (sa !== sb) return sa - sb;
    const ta = String(a.title ?? a.addonId ?? a.addon_id ?? '').toLowerCase();
    const tb = String(b.title ?? b.addonId ?? b.addon_id ?? '').toLowerCase();
    return ta.localeCompare(tb);
  });
}

const AppsHomeScreen = {
  render() {
    return `
      <div class="apps-greeting">
        <img class="mascot" src="/tentaflow.png" alt="">
        <h1 id="apps-greeting-h"></h1>
        <div class="hi">${escapeHtml(I18n.t('apps_home.subtitle'))}</div>
      </div>
      <div class="apps-grid" id="apps-grid"></div>`;
  },
  async mount() {
    // Greeting + role gate use the real authMeRequest (no stub). The role
    // check matches app.js: power = 'power_user' or 'admin' (admin is a
    // superset). Built-in tiles render only after the role is known, so a
    // gated tile never exists in the DOM for a regular user.
    let me = null;
    try {
      me = await ApiBinary.one('authMeRequest');
    } catch {}
    byId('apps-greeting-h').textContent = I18n.t('apps_home.greeting', { name: me?.username ?? '' });
    const role = (me?.role ?? 'user').toLowerCase();
    const isPowerUser = role === 'admin' || role === 'power_user';

    const grid = byId('apps-grid');
    grid.innerHTML = TILES
      .filter((t) => !t.requiresPowerUser || isPowerUser)
      .map(renderTile)
      .join('');

    // Dolacz kafelki z zunifikowanej listy serwerowej (native + WASM,
    // przefiltrowanej po widocznosci/enable/uprawnieniach). Bledem nie
    // zabijamy calego widoku — kafelki built-in zostaja widoczne.
    try {
      const apps = await ApiBinary.list('appsListRequest', { arrayKey: 'apps' });
      if (Array.isArray(apps) && apps.length > 0) {
        const html = sortAddonApps(apps).map(renderAddonTile).join('');
        grid.insertAdjacentHTML('beforeend', html);
      }
    } catch (e) {
      console.warn('[apps-home] apps list fetch failed:', e?.message ?? e);
    }

    grid.querySelectorAll('.app-tile').forEach((el) => {
      el.addEventListener('click', () => {
        // Server-driven tile: native -> Router screen, wasm -> UI v2 renderer.
        if (el.classList.contains('addon-app-tile')) {
          if (el.dataset.enabled === '0') return;
          const target = el.dataset.target;
          if (!target) return;
          if (el.dataset.kind === 'native') {
            Router.navigate(target);
          } else {
            Router.navigate('addon-app', { addonId: el.dataset.addonId, panelId: target });
          }
          return;
        }
        const route = el.dataset.route;
        if (route) Router.navigate(route);
      });
    });
  },
  unmount() {},
};

export default AppsHomeScreen;
