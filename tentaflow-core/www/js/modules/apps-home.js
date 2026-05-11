// =============================================================================
// File: modules/apps-home.js — User home: greeting banner + tiled apps grid.
// Rendered as the default screen for role=user. Each tile navigates via Router.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { Router } from '/js/router.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeHtml } from '/js/utils.js';

// App tiles. Tiles whose backend handlers are not yet wired carry `soon: true`
// and render as non-navigable placeholders (kept in sync with app.js USER_NAV).
const TILES = [
  { id: 'chat',         route: 'chat',         icon: 'chat' },
  { id: 'images',       route: 'images',       icon: 'image',        soon: true },
  { id: 'notes',        route: 'notes',        icon: 'mic',          soon: true },
  { id: 'meeting',      route: 'meeting',      icon: 'meeting',      soon: true },
  { id: 'pose',         route: 'pose',         icon: 'image' },
  { id: 'translate',    route: 'translate',    icon: 'globe',        soon: true },
];

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function renderTile(t) {
  const name = escapeHtml(I18n.t(`apps.${t.id}.name`));
  const desc = escapeHtml(I18n.t(`apps.${t.id}.desc`));
  const badge = t.soon ? `<span class="badge-soon">${escapeHtml(I18n.t('apps.badge_soon'))}</span>` : '';
  const cls = `app-tile${t.soon ? ' coming-soon' : ''}`;
  return `
    <div class="${cls}" data-route="${escapeHtml(t.route)}" data-soon="${t.soon ? '1' : '0'}">
      ${badge}
      <div class="app-icon">${sprite(t.icon)}</div>
      <div class="app-name">${name}</div>
      <div class="app-desc">${desc}</div>
    </div>`;
}

// Dynamic tile dla zainstalowanego addonu z `[application]` w manifescie.
// Click -> Router.navigate('addon-app', { addonId, panelId }).
function renderAddonTile(app) {
  const title = escapeHtml(app.title ?? app.addonId ?? '');
  const desc = escapeHtml(app.addonId ?? '');
  const iconRaw = (app.icon ?? '').toString();
  const iconId = (iconRaw.startsWith('i-') ? iconRaw.slice(2) : iconRaw) || 'chip';
  return `
    <div class="app-tile addon-app-tile"
         data-addon-id="${escapeHtml(app.addonId ?? '')}"
         data-panel-id="${escapeHtml(app.entryPanel ?? '')}">
      <div class="app-icon">${sprite(iconId)}</div>
      <div class="app-name">${title}</div>
      <div class="app-desc">${desc}</div>
    </div>`;
}

const AppsHomeScreen = {
  render() {
    return `
      <div class="apps-greeting">
        <img class="mascot" src="/tentaflow.png" alt="">
        <h1 id="apps-greeting-h"></h1>
        <div class="hi">${escapeHtml(I18n.t('apps_home.subtitle'))}</div>
      </div>
      <div class="apps-grid" id="apps-grid">
        ${TILES.map(renderTile).join('')}
      </div>`;
  },
  async mount() {
    // Greeting uses the real username from authMeRequest (no stub).
    try {
      const me = await ApiBinary.one('authMeRequest');
      const name = me?.username ?? '';
      byId('apps-greeting-h').textContent = I18n.t('apps_home.greeting', { name });
    } catch {
      byId('apps-greeting-h').textContent = I18n.t('apps_home.greeting', { name: '' });
    }

    const grid = byId('apps-grid');

    // Dolacz dynamiczne kafelki addon applications. Bledem nie zabijamy
    // calego widoku — kafelki built-in zostaja widoczne.
    try {
      const apps = await ApiBinary.list('addonApplicationsListRequest', {
        arrayKey: 'applications',
      });
      if (Array.isArray(apps) && apps.length > 0) {
        const html = apps.map(renderAddonTile).join('');
        grid.insertAdjacentHTML('beforeend', html);
      }
    } catch (e) {
      console.warn('[apps-home] addon applications fetch failed:', e?.message ?? e);
    }

    grid.querySelectorAll('.app-tile').forEach((el) => {
      el.addEventListener('click', () => {
        // Addon app tile — drill-down do renderera UI v2.
        if (el.classList.contains('addon-app-tile')) {
          const addonId = el.dataset.addonId;
          const panelId = el.dataset.panelId;
          if (addonId && panelId) {
            Router.navigate('addon-app', { addonId, panelId });
          }
          return;
        }
        // Soon tiles still navigate — the target screen explains the status
        // honestly instead of faking a feature.
        const route = el.dataset.route;
        if (route) Router.navigate(route);
      });
    });
  },
  unmount() {},
};

export default AppsHomeScreen;
