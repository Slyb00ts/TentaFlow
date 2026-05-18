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

// Whitelista ikon — patrz app.js. Trzymamy lokalny fallback bez importu zeby
// nie tworzyc cyklu, ale zachowujemy spojnosc semantyki: nieznana ikona => 'apps'.
function resolveIcon(raw) {
  const t = String(raw ?? '').trim();
  const id = t.startsWith('i-') ? t.slice(2) : t;
  return id || 'apps';
}

// Dynamic tile dla zainstalowanego addonu z `[application]` w manifescie.
// Click -> Router.navigate('addon-app', { addonId, panelId }).
function renderAddonTile(app) {
  const addonId = app.addonId ?? app.addon_id ?? '';
  const panelId = app.entryPanel ?? app.entry_panel ?? '';
  const title = escapeHtml(app.title ?? addonId);
  const desc = escapeHtml(app.description ?? addonId);
  const iconId = resolveIcon(app.icon);
  const enabled = app.enabled !== false;
  const disabledBadge = enabled
    ? ''
    : `<span class="badge-soon">${escapeHtml(I18n.t('addon.disabled') || 'disabled')}</span>`;
  const cls = `app-tile addon-app-tile${enabled ? '' : ' coming-soon'}`;
  return `
    <div class="${cls}"
         data-addon-id="${escapeHtml(addonId)}"
         data-panel-id="${escapeHtml(panelId)}"
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
        const html = sortAddonApps(apps).map(renderAddonTile).join('');
        grid.insertAdjacentHTML('beforeend', html);
      }
    } catch (e) {
      console.warn('[apps-home] addon applications fetch failed:', e?.message ?? e);
    }

    grid.querySelectorAll('.app-tile').forEach((el) => {
      el.addEventListener('click', () => {
        // Addon app tile — drill-down do renderera UI v2.
        if (el.classList.contains('addon-app-tile')) {
          if (el.dataset.enabled === '0') return;
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
