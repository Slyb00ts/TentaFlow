// =============================================================================
// File: modules/addons.js
// Description: Addons screen. Grid of installed addons + drill-down detail view
//              with tabs: Settings, Visibility, Permissions, OAuth, Linked
//              accounts, Logs, Tools. Admin-only tabs hidden for non-admins.
//              Detail opened via AddonsScreen.showDetail(addonId). Install ZIP
//              goes through binary AddonInstallRequest.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast, formatBytes } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

import { VisibilityTab } from '/js/modules/addons/visibility.js';
import { PermissionsTab } from '/js/modules/addons/permissions.js';
import { AccessTab } from '/js/modules/addons/access.js';
import { OAuthConfigTab } from '/js/modules/addons/oauth-config.js';
import { LinkedAccountsTab } from '/js/modules/addons/linked-accounts.js';
import { SettingsTab } from '/js/modules/addons/settings.js';
import { LogsTab } from '/js/modules/addons/logs.js';
import { ToolsTab } from '/js/modules/addons/tools.js';
import { ResourcesTab } from '/js/modules/addons/resources.js';
import { NetworkTab } from '/js/modules/addons/network.js';
import { BindingsTab } from '/js/modules/addons/bindings.js';
import { openUninstallDialog } from '/js/modules/addons/uninstall-dialog.js';
// `openInstallWizard` reserved for the future "Install from ZIP" flow on the
// addons list page; it is no longer triggered from the per-addon header.

// --- Stan listy ------------------------------------------------------------
let addonsList = [];
let catalogList = [];           // AddonPackageInfo[] (zakladka "Katalog")
let activeView = 'installed';   // installed | catalog
let filterSearch = '';
let filterMode = 'all';  // all | enabled | disabled | oauth_global | oauth_individual | runtime_*
let isAdmin = false;

// --- Stan detail -----------------------------------------------------------
let currentAddonId = null;
let currentAddonDetail = null;  // AddonDetailResponse
let activeTab = 'settings';
let activeTabController = null;  // {unmount}

function sprite(id) { return `<svg class="icon"><use href="#i-${id}"/></svg>`; }

// Renders a sprite using a raw id (already `i-*`) or bare name; normalizes by
// stripping an optional leading `i-` so it composes with the `#i-` href prefix.
function spriteRaw(id) {
  const name = String(id || '').replace(/^i-/, '');
  return `<svg class="icon"><use href="#i-${name}"/></svg>`;
}

// Maps a manifest category to a default sprite id when the manifest omits `icon`.
// Categories are stable labels from `[addon].category` — see SCHEMA.md.
function iconForCategory(category) {
  switch ((category || '').toLowerCase()) {
    case 'communication': return 'i-meeting';
    case 'storage': return 'i-database';
    case 'filter': return 'i-shield';
    case 'transform': return 'i-transform';
    default: return 'i-puzzle';
  }
}

function renderAddonBadge(label, { status = 'info', icon = '', className = '' } = {}) {
  const classes = ['addon-badge'];
  if (className) classes.push(className);
  const iconAttr = icon ? ` icon="${escapeAttr(icon)}"` : '';
  return `<tf-chip variant="addon-badge" class="${escapeAttr(classes.join(' '))}" status="${escapeAttr(status)}"${iconAttr}>${escapeHtml(label)}</tf-chip>`;
}

function formatVisibilityBadge(visibilityScope) {
  if (!visibilityScope) return '';
  if (visibilityScope === 'all_groups') {
    return renderAddonBadge(I18n.t('addons.badges.visibility_all'), {
      status: 'info',
      icon: 'users',
      className: 'visibility',
    });
  }
  if (visibilityScope === 'admin_only') {
    return renderAddonBadge(I18n.t('addons.badges.visibility_admin_only'), {
      status: 'warn',
      icon: 'users',
      className: 'visibility',
    });
  }
  const match = /^(\d+)_groups$/.exec(visibilityScope);
  if (match) {
    return renderAddonBadge(I18n.t('addons.badges.visibility_n_groups', { n: Number(match[1]) }), {
      status: 'warn',
      icon: 'users',
      className: 'visibility',
    });
  }
  return renderAddonBadge(visibilityScope, {
    status: 'info',
    icon: 'users',
    className: 'visibility',
  });
}

function footerTextForAddon({ enabled, usersWithOauth, oauthMode, runtime }) {
  if (!enabled) return I18n.t('addons.not_loaded');
  if (usersWithOauth > 0) {
    return I18n.t('addons.badges.users_with_oauth', { n: usersWithOauth });
  }
  if (oauthMode === 'global') return I18n.t('addons.shared_account');
  if (oauthMode === 'none') return I18n.t('addons.oauth_disabled');
  return runtime || '—';
}

const AddonsScreen = {
  get title() { return I18n.t('addons.list_title'); },

  render() {
    return `
      <div class="addons-screen">
        <div class="page-header">
          <div>
            <h1>${sprite('puzzle')} ${escapeHtml(I18n.t('addons.list_title'))}</h1>
            <div class="sub" id="addons-sub">${escapeHtml(I18n.t('common.loading'))}</div>
          </div>
          <div class="actions">
            <tf-button variant="secondary" icon="globe" id="addons-browse">${escapeHtml(I18n.t('addons.browse_registry'))}</tf-button>
            <tf-button variant="primary" icon="upload" id="addons-install">${escapeHtml(I18n.t('addons.install_zip'))}</tf-button>
          </div>
        </div>

        <div class="tf-filter-group addons-view-switch" role="tablist" style="margin-bottom:10px;">
          <tf-chip class="view-chip" clickable active data-view="installed" icon="puzzle">Zainstalowane</tf-chip>
          <tf-chip class="view-chip" clickable data-view="catalog" icon="globe">Katalog</tf-chip>
        </div>

        <div class="addons-toolbar">
          <tf-searchbox class="addons-search" id="addons-search" placeholder="${escapeAttr(I18n.t('addons.search_placeholder'))}" debounce="200"></tf-searchbox>
          <div class="tf-filter-group" role="tablist">
            <tf-chip class="filter-chip" clickable active data-f="all">${escapeHtml(I18n.t('addons.filter_all'))}</tf-chip>
            <tf-chip class="filter-chip" clickable data-f="enabled">${escapeHtml(I18n.t('addons.enabled'))}</tf-chip>
            <tf-chip class="filter-chip" clickable data-f="disabled">${escapeHtml(I18n.t('addons.disabled'))}</tf-chip>
            <tf-chip class="filter-chip" clickable icon="globe" data-f="oauth_global">${escapeHtml(I18n.t('addons.oauth_global_filter'))}</tf-chip>
            <tf-chip class="filter-chip" clickable icon="user" data-f="oauth_individual">${escapeHtml(I18n.t('addons.oauth_individual_filter'))}</tf-chip>
            <tf-chip class="filter-chip" clickable icon="chip" data-f="runtime_wasmtime">${escapeHtml(I18n.t('addons.badges.runtime_wasmtime'))}</tf-chip>
            <tf-chip class="filter-chip" clickable icon="chip" data-f="runtime_wasmi">${escapeHtml(I18n.t('addons.badges.runtime_wasmi'))}</tf-chip>
            <tf-chip class="filter-chip" clickable icon="chip" data-f="runtime_native">${escapeHtml(I18n.t('addons.badges.runtime_native'))}</tf-chip>
          </div>
        </div>

        <div id="addons-grid" class="addon-grid"></div>
      </div>
    `;
  },

  async mount() {
    await detectRole();
    await loadList();
    await loadCatalog();
    attachListHandlers();
  },

  unmount() {
    unmountActiveTab();
    addonsList = [];
    catalogList = [];
    activeView = 'installed';
    currentAddonId = null;
    currentAddonDetail = null;
    activeTab = 'settings';
  },

  // Public API invoked from card list or router.
  async showDetail(addonId) {
    currentAddonId = addonId;
    activeTab = 'settings';
    await detectRole();
    const host = byId('main');
    if (!host) return;
    host.innerHTML = renderDetailSkeleton();
    await loadDetail();
    renderDetail();
  },
};

// --- Role detection --------------------------------------------------------
async function detectRole() {
  try {
    const me = await ApiBinary.one('authMeRequest');
    isAdmin = String(me.role || '').toLowerCase() === 'admin';
  } catch {
    isAdmin = false;
  }
}

// --- Lista -----------------------------------------------------------------
async function loadList() {
  try {
    addonsList = await ApiBinary.list('addonsListRequest', { arrayKey: 'addons' });
    if (activeView === 'installed') {
      renderList();
      updateSubtitle();
    }
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// Katalog pakietow (szablonow). Kazdy pakiet ma akcje "Zainstaluj" tworzaca
// nowa instancje pod nadana nazwa.
async function loadCatalog() {
  try {
    catalogList = await ApiBinary.list('addonCatalogListRequest', { arrayKey: 'packages' });
    if (activeView === 'catalog') renderCatalog();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// Renderuje aktywny widok (zainstalowane albo katalog) do wspolnego gridu.
function renderActive() {
  if (activeView === 'catalog') {
    renderCatalog();
  } else {
    renderList();
    updateSubtitle();
  }
}

function updateSubtitle() {
  const sub = byId('addons-sub');
  if (!sub) return;
  const total = addonsList.length;
  const enabled = addonsList.filter((a) => !!(a.isEnabled ?? a.is_enabled)).length;
  const oauth = addonsList.filter((a) => {
    const mode = a.oauthMode ?? a.oauth_mode ?? null;
    return mode !== null && mode !== 'none';
  }).length;
  const runtimes = [...new Set(
    addonsList
      .map((a) => String(a.runtime || '').trim())
      .filter(Boolean),
  )].sort().join('/');
  sub.textContent = I18n.t('addons.cards_count', {
    total,
    enabled,
    oauth,
    runtimes: runtimes || '—',
  });
}

function attachListHandlers() {
  // Przelacznik widoku Zainstalowane / Katalog.
  document.querySelectorAll('.addons-view-switch tf-chip[data-view]').forEach((chip) => {
    chip.addEventListener('click', () => {
      document
        .querySelectorAll('.addons-view-switch tf-chip[data-view]')
        .forEach((c) => c.removeAttribute('active'));
      chip.setAttribute('active', '');
      activeView = chip.dataset.view;
      renderActive();
    });
  });
  byId('addons-search')?.addEventListener('search', (e) => {
    filterSearch = (e.detail?.value || '').toLowerCase();
    renderActive();
  });
  document.querySelectorAll('.addons-screen tf-chip[clickable][data-f]').forEach((chip) => {
    chip.addEventListener('click', () => {
      document.querySelectorAll('.addons-screen tf-chip[clickable][data-f]').forEach((c) => c.removeAttribute('active'));
      chip.setAttribute('active', '');
      filterMode = chip.dataset.f;
      renderActive();
    });
  });
  byId('addons-install')?.addEventListener('click', onInstallZip);
  byId('addons-browse')?.addEventListener('click', () => {
    activeView = 'catalog';
    document
      .querySelectorAll('.addons-view-switch tf-chip[data-view]')
      .forEach((c) => c.toggleAttribute('active', c.dataset.view === 'catalog'));
    renderActive();
  });
}

function renderList() {
  const grid = byId('addons-grid');
  if (!grid) return;

  const filtered = addonsList.filter((a) => {
    if (filterSearch) {
      const dn = a.displayName ?? a.display_name ?? '';
      const hay = `${dn} ${a.name} ${a.addonId ?? a.addon_id} ${a.description || ''} ${a.category || ''} ${a.runtime || ''}`.toLowerCase();
      if (!hay.includes(filterSearch)) return false;
    }
    const runtime = String(a.runtime || '').toLowerCase();
    if (filterMode === 'enabled' && !(a.isEnabled ?? a.is_enabled)) return false;
    if (filterMode === 'disabled' && (a.isEnabled ?? a.is_enabled)) return false;
    if (filterMode === 'oauth_global' && (a.oauthMode ?? a.oauth_mode) !== 'global') return false;
    if (filterMode === 'oauth_individual' && (a.oauthMode ?? a.oauth_mode) !== 'individual') return false;
    if (filterMode.startsWith('runtime_') && runtime !== filterMode.slice('runtime_'.length)) return false;
    return true;
  });

  if (filtered.length === 0) {
    grid.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('addons.empty'))}</div>`;
    return;
  }

  grid.innerHTML = filtered.map((a) => renderCard(a)).join('');
  grid.querySelectorAll('[data-addon-card]').forEach((card) => {
    card.addEventListener('click', () => {
      AddonsScreen.showDetail(card.dataset.addonCard);
    });
  });
  // Toggle enabled — binary handler AddonToggleRequest wlacza/wylacza addon.
  grid.querySelectorAll('tf-toggle[data-role="enabled"]').forEach((t) => {
    t.addEventListener('click', (e) => e.stopPropagation());
    t.addEventListener('change', async (e) => {
      e.stopPropagation();
      const card = t.closest('.addon-card');
      const addonId = card?.dataset.addonCard;
      if (!addonId) return;
      const enabled = !!(e.detail?.checked ?? t.hasAttribute('checked'));
      try {
        await ApiBinary.action('addonToggleRequest', { addonId, enabled });
        toast(I18n.t(enabled ? 'addon_toggle.success_enabled' : 'addon_toggle.success_disabled'), 'success');
        // Zaktualizuj lokalny model, zeby filtr enabled/disabled pokazal spojny stan.
        const entry = addonsList.find((a) => (a.addonId ?? a.addon_id) === addonId);
        if (entry) {
          if ('isEnabled' in entry) entry.isEnabled = enabled;
          else entry.is_enabled = enabled;
        }
      } catch (err) {
        // Rollback wizualny gdy backend odmowil.
        if (enabled) t.removeAttribute('checked'); else t.setAttribute('checked', '');
        toast(`${I18n.t('addon_toggle.error')}: ${err.message}`, 'error');
      }
    });
  });
  // Akcje instancji (update/duplicate/uninstall) — stopPropagation, bo karta
  // otwiera detal.
  grid.querySelectorAll('.a-actions tf-button[data-act]').forEach((btn) => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      const id = btn.dataset.id;
      switch (btn.dataset.act) {
        case 'update': openUpdateModal(id); break;
        case 'duplicate': openDuplicateModal(id); break;
        case 'uninstall': onUninstallInstance(id, btn.dataset.name || id); break;
      }
    });
  });
}

// --- Katalog ---------------------------------------------------------------
function renderCatalog() {
  const grid = byId('addons-grid');
  if (!grid) return;
  const filtered = catalogList.filter((p) => {
    if (!filterSearch) return true;
    const hay = `${p.packageId ?? p.package_id} ${p.name || ''}`.toLowerCase();
    return hay.includes(filterSearch);
  });
  const sub = byId('addons-sub');
  if (sub) sub.textContent = `${catalogList.length} pakiet(ow) w katalogu`;
  if (filtered.length === 0) {
    grid.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('addons.empty'))}</div>`;
    return;
  }
  grid.innerHTML = filtered.map((p) => renderCatalogCard(p)).join('');
  grid.querySelectorAll('tf-button[data-act="install"]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const pkgId = btn.dataset.pkg;
      const pkg = catalogList.find((p) => (p.packageId ?? p.package_id) === pkgId);
      if (pkg) openInstallInstanceModal(pkg);
    });
  });
}

function renderCatalogCard(p) {
  const pkgId = p.packageId ?? p.package_id;
  const latest = p.latestVersion ?? p.latest_version ?? '0.0.0';
  const versions = Array.isArray(p.versions) ? p.versions : [];
  const installed = Number(p.installedInstances ?? p.installed_instances ?? 0);
  const source = p.source || 'bundled';
  const badges = [];
  badges.push(renderAddonBadge(`v${latest}`, { status: 'info', icon: 'chip', className: 'runtime' }));
  if (versions.length > 1) {
    badges.push(renderAddonBadge(`${versions.length} wersji`, { status: 'accent', icon: 'history', className: 'perms' }));
  }
  if (installed > 0) {
    badges.push(renderAddonBadge(I18n.t('addons.badges.instances_count', { n: installed }), { status: 'ok', icon: 'puzzle', className: 'visibility' }));
  }
  badges.push(renderAddonBadge(source, { status: 'info', icon: 'globe', className: 'oauth-generic' }));
  // A singleton package has ONE instance fleet-wide, so offering "install"
  // again would only produce a server-side refusal; a multi-instance package
  // says outright that the button adds another instance.
  const singleton = !!(p.singleton ?? false);
  const installedSingleton = singleton && installed > 0;
  let installBtn = '';
  if (isAdmin && !installedSingleton) {
    const label = I18n.t(installed > 0 ? 'addons.install_another' : 'addons.install_action');
    installBtn = `<tf-button size="sm" variant="primary" icon="download" data-act="install" data-pkg="${escapeAttr(pkgId)}">${escapeHtml(label)}</tf-button>`;
  } else if (isAdmin) {
    installBtn = `<span class="a-foot-note">${escapeHtml(I18n.t('addons.install_singleton_done'))}</span>`;
  }
  return `
    <div class="addon-card" data-catalog-card="${escapeAttr(pkgId)}">
      <div class="addon-head">
        <div class="addon-ico">${spriteRaw('puzzle')}</div>
        <div class="addon-meta">
          <div class="a-name">${escapeHtml(p.name || pkgId)}</div>
          <div class="a-version">${escapeHtml(pkgId)}</div>
        </div>
      </div>
      <div class="a-badges">${badges.join('')}</div>
      <div class="a-actions" style="display:flex;gap:6px;margin-top:8px;">${installBtn}</div>
    </div>
  `;
}

function renderCard(a) {
  const id = a.addonId ?? a.addon_id;
  const enabled = !!(a.isEnabled ?? a.is_enabled);
  const isSystem = !!(a.isSystem ?? a.is_system);
  const runtime = a.runtime || '';
  const oauthMode = a.oauthMode ?? a.oauth_mode ?? null;
  const visibilityScope = a.visibilityScope ?? a.visibility_scope ?? '';
  const permissionsCount = Number(a.declaredPermissionsCount ?? a.declared_permissions_count ?? 0);
  const usersWithOauth = Number(a.usersWithOauthCount ?? a.users_with_oauth_count ?? 0);
  const category = a.category ?? null;
  const iconId = a.icon || iconForCategory(category);
  const sizeBytes = Number(a.fileSizeBytes ?? 0);
  const sizeLabel = sizeBytes > 0 ? formatBytes(sizeBytes) : '';

  const metaParts = [];
  const pkgVersion = a.packageVersion ?? a.package_version ?? a.version ?? '0.0.0';
  metaParts.push(`v${escapeHtml(pkgVersion)}`);
  if (sizeLabel) metaParts.push(escapeHtml(sizeLabel));
  if (runtime) metaParts.push(escapeHtml(runtime));
  if (a.author) metaParts.push(escapeHtml(a.author));
  const versionLine = metaParts.join(' · ');
  const badges = [];
  if (oauthMode) {
    const badgeConfig = {
      global: { status: 'ok', icon: 'globe', className: 'oauth-global' },
      individual: { status: 'accent', icon: 'user', className: 'oauth-individual' },
      none: { status: 'warn', icon: 'x', className: 'oauth-none' },
      mixed: { status: 'warn', icon: 'share', className: 'oauth-mixed' },
    };
    const config = badgeConfig[oauthMode] || { status: 'info', icon: 'key', className: 'oauth-generic' };
    badges.push(renderAddonBadge(I18n.t('addons.badges.oauth_' + oauthMode), config));
  }
  const updateAvailable = !!(a.updateAvailable ?? a.update_available);
  if (updateAvailable) {
    badges.push(renderAddonBadge('aktualizacja', {
      status: 'warn',
      icon: 'download',
      className: 'update-available',
    }));
  }
  const visibilityBadge = formatVisibilityBadge(visibilityScope);
  if (visibilityBadge) badges.push(visibilityBadge);
  if (permissionsCount > 0) {
    badges.push(renderAddonBadge(I18n.t('addons.badges.permissions_count', { n: permissionsCount }), {
      status: 'accent',
      icon: 'shield',
      className: 'perms',
    }));
  }
  if (isSystem) {
    badges.push(renderAddonBadge(I18n.t('addons.system'), {
      status: 'info',
      icon: 'chip',
      className: 'runtime',
    }));
  }
  // Native apps: compiled into core, versioned with it — the badge tells the
  // admin why there is no separate update/duplicate lifecycle.
  if (runtime === 'native') {
    badges.push(renderAddonBadge(I18n.t('addons.badges.runtime_native'), {
      status: 'accent',
      icon: 'chip',
      className: 'runtime native',
    }));
  }
  const footerText = footerTextForAddon({ enabled, usersWithOauth, oauthMode, runtime });
  const displayName = a.displayName ?? a.display_name ?? a.name ?? id;
  // background_on_disable apps keep serving (shares, schedules) while
  // disabled: the toggle must not read as a full stop.
  const backgroundOnDisable = !!(a.backgroundOnDisable ?? a.background_on_disable);
  const toggleHint = backgroundOnDisable ? I18n.t('addons.background_on_disable_hint') : '';
  // Wiersz akcji instancji (admin). stopPropagation w handlerze, zeby klik w
  // przycisk nie otwieral detalu.
  const actions = isAdmin
    ? `
      <div class="a-actions" style="display:flex;gap:6px;flex-wrap:wrap;margin-top:8px;">
        ${updateAvailable ? `<tf-button size="sm" variant="primary" icon="download" data-act="update" data-id="${escapeAttr(id)}">Aktualizuj</tf-button>` : ''}
        ${runtime === 'native' ? '' : `<tf-button size="sm" variant="secondary" icon="copy" data-act="duplicate" data-id="${escapeAttr(id)}">Duplikuj</tf-button>`}
        <tf-button size="sm" variant="ghost" icon="trash" data-act="uninstall" data-id="${escapeAttr(id)}" data-name="${escapeAttr(displayName)}">Odinstaluj</tf-button>
      </div>`
    : '';

  return `
    <div class="addon-card${enabled ? '' : ' disabled'}" data-addon-card="${escapeAttr(id)}">
      <div class="addon-head">
        <div class="addon-ico">${spriteRaw(iconId)}</div>
        <div class="addon-meta">
          <div class="a-name">${escapeHtml(displayName)}</div>
          <div class="a-version">${versionLine}</div>
        </div>
      </div>
      <div class="a-desc">${escapeHtml(a.description || '')}</div>
      <div class="a-badges">
        ${badges.join('')}
      </div>
      <div class="a-foot">
        <span class="a-status ${enabled ? 'on' : 'off'}">● ${escapeHtml(I18n.t(enabled ? 'addons.enabled' : 'addons.disabled').toLowerCase())}</span>
        <span class="a-sep">·</span>
        <span class="a-foot-note">${escapeHtml(footerText)}</span>
        <tf-toggle size="sm" data-role="enabled" ${enabled ? 'checked' : ''}${toggleHint ? ` title="${escapeAttr(toggleHint)}"` : ''}></tf-toggle>
      </div>
      ${toggleHint ? `<div class="a-toggle-hint"><svg class="icon"><use href="#i-info"/></svg>${escapeHtml(toggleHint)}</div>` : ''}
      ${actions}
    </div>
  `;
}

// Generyczny modal TfWindow z body + przyciskiem potwierdzenia. onConfirm(win)
// moze byc async; gdy zwroci falsy/throw, okno zostaje otwarte (blad pokazany).
function openModal({ title, icon = 'puzzle', bodyHtml, confirmLabel, confirmIcon = 'check', onConfirm }) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', title);
  win.setAttribute('icon', icon);
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '420');
  win.setAttribute('width', '460');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = bodyHtml;
  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.innerHTML = `
    <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
    <tf-button variant="primary" icon="${escapeAttr(confirmIcon)}" data-action="confirm">${escapeHtml(confirmLabel)}</tf-button>
  `;
  win.appendChild(body);
  win.appendChild(foot);
  win.addEventListener('action', async (e) => {
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    try {
      const ok = await onConfirm(win);
      if (ok !== false) win.remove();
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });
  document.body.appendChild(win);
  return win;
}

// Katalog → "Zainstaluj": nazwa instancji + wybor wersji pakietu.
function openInstallInstanceModal(pkg) {
  const pkgId = pkg.packageId ?? pkg.package_id;
  const versions = Array.isArray(pkg.versions) ? pkg.versions : [];
  const opts = versions.map((v) => `<option value="${escapeAttr(v)}">${escapeHtml(v)}</option>`).join('');
  // Connection params declared by the package ([[robot.connection_param]]).
  // Each renders a tf-input collected into the install request's config map, so
  // robot packages get a per-instance IP/serial instead of a hardcoded default.
  const params = Array.isArray(pkg.connectionParams ?? pkg.connection_params)
    ? (pkg.connectionParams ?? pkg.connection_params)
    : [];
  const paramType = (p) => (p.paramType ?? p.param_type ?? '').toLowerCase();
  const paramFields = params.map((p) => {
    const key = p.key;
    const label = p.label || key;
    const required = !!p.required;
    const placeholder = p.placeholder || '';
    const reqMark = required ? ' <span style="color:var(--danger);">*</span>' : '';
    return `<label style="display:flex;flex-direction:column;gap:4px;">${escapeHtml(label)}${reqMark}
          <tf-input data-param-key="${escapeAttr(key)}" data-param-type="${escapeAttr(paramType(p))}" placeholder="${escapeAttr(placeholder)}"></tf-input>
        </label>`;
  }).join('');
  // Multi-instance package: the name is what tells two instances apart on the
  // apps grid and in the permission matrix, so it is required AND must differ
  // from the names already taken by this package's instances.
  const singleton = !!(pkg.singleton ?? false);
  const takenNames = addonsList
    .filter((a) => (a.packageId ?? a.package_id) === pkgId)
    .map((a) => String(a.displayName ?? a.display_name ?? a.name ?? '').trim().toLowerCase());
  openModal({
    title: I18n.t('addons.install_title', { name: pkg.name || pkgId }),
    icon: 'download',
    confirmLabel: I18n.t(takenNames.length > 0 ? 'addons.install_another' : 'addons.install_action'),
    confirmIcon: 'download',
    bodyHtml: `
      <div style="display:flex;flex-direction:column;gap:12px;font-size:13px;">
        ${singleton ? '' : `<div style="color:var(--text-2);">${escapeHtml(I18n.t('addons.instance_name_hint'))}</div>`}
        <label style="display:flex;flex-direction:column;gap:4px;">${escapeHtml(I18n.t('addons.instance_name'))} <span style="color:var(--danger);">*</span>
          <tf-input id="inst-name" placeholder="${escapeAttr(pkg.name || pkgId)}" required></tf-input>
        </label>
        <label style="display:flex;flex-direction:column;gap:4px;">${escapeHtml(I18n.t('addons.instance_version'))}
          <tf-select id="inst-version">${opts}</tf-select>
        </label>
        ${paramFields}
      </div>`,
    onConfirm: async (win) => {
      const name = (win.querySelector('#inst-name')?.value || '').trim();
      const version = win.querySelector('#inst-version')?.value || versions[0];
      if (!name) { toast(I18n.t('addons.instance_name_required'), 'error'); return false; }
      if (takenNames.includes(name.toLowerCase())) {
        toast(I18n.t('addons.instance_name_taken', { name }), 'error');
        return false;
      }
      const config = [];
      for (const p of params) {
        const el = win.querySelector(`tf-input[data-param-key="${CSS.escape(p.key)}"]`);
        // Trim regardless of type — leading/trailing whitespace is never a valid
        // connection-param value and the server trims too.
        const value = (el?.value || '').trim();
        if (p.required && !value) {
          toast(`Pole "${p.label || p.key}" jest wymagane`, 'error');
          return false;
        }
        // Operator-feedback gate mirroring the server host rule (the server is the
        // real gate). A host-type param must be a bare host: no spaces or scheme.
        if (value && paramType(p) === 'host') {
          if (/\s/.test(value) || /:\/\//.test(value)) {
            toast(`Pole "${p.label || p.key}": podaj sam adres IP lub nazwe hosta (bez schematu i spacji)`, 'error');
            return false;
          }
        }
        if (value) config.push([p.key, value]);
      }
      const res = await ApiBinary.action('addonInstanceInstallRequest', {
        packageId: pkgId,
        version,
        displayName: name,
        config,
      });
      if (!res.ok) { toast(res.error || I18n.t('addons.install_failed'), 'error'); return false; }
      toast(I18n.t('addons.install_done', { name }), 'success');
      await refreshAll();
      return true;
    },
  });
}

// Instancja → "Aktualizuj": pobierz wersje, wybierz docelowa, hot-update.
async function openUpdateModal(addonId) {
  let info;
  try {
    info = await ApiBinary.one('addonInstanceVersionsRequest', { addonId });
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    return;
  }
  const current = info.current || '';
  const available = Array.isArray(info.available) ? info.available : [];
  // Newer versions come first (catalog is sorted newest-first). The current
  // version stays selectable as a "reload changed content" option: a bundled
  // addon often ships edits under the SAME version, so re-applying it refreshes
  // the manifest (permissions, storage) without a version bump.
  const hasNewer = available.some((v) => v !== current);
  const opts = available
    .map((v) => {
      const label = v === current ? `${escapeHtml(v)} (przeładuj zawartość)` : escapeHtml(v);
      const selected = (hasNewer ? v !== current : v === current) ? ' selected' : '';
      return `<option value="${escapeAttr(v)}"${selected}>${label}</option>`;
    })
    .join('');
  openModal({
    title: `Aktualizuj instancje`,
    icon: 'download',
    confirmLabel: 'Aktualizuj',
    confirmIcon: 'download',
    bodyHtml: `
      <div style="display:flex;flex-direction:column;gap:12px;font-size:13px;">
        <div style="color:var(--text-2);">Aktualna wersja: <b>${escapeHtml(current)}</b></div>
        <label style="display:flex;flex-direction:column;gap:4px;">Wersja docelowa
          <tf-select id="upd-version">${opts}</tf-select>
        </label>
      </div>`,
    onConfirm: async (win) => {
      const target = win.querySelector('#upd-version')?.value || '';
      if (!target) { toast('Wybierz wersje', 'error'); return false; }
      const res = await ApiBinary.action('addonInstanceUpdateRequest', { addonId, targetVersion: target });
      if (!res.ok) { toast(res.error || 'Aktualizacja nieudana', 'error'); return false; }
      toast(`Zaktualizowano do v${target}`, 'success');
      await refreshAll();
      return true;
    },
  });
}

// Instancja → "Duplikuj": nowa nazwa, pusta kopia.
function openDuplicateModal(sourceAddonId) {
  openModal({
    title: 'Duplikuj instancje',
    icon: 'copy',
    confirmLabel: 'Duplikuj',
    confirmIcon: 'copy',
    bodyHtml: `
      <div style="display:flex;flex-direction:column;gap:12px;font-size:13px;">
        <div style="color:var(--text-2);">Nowa instancja tego samego pakietu, z pustymi danymi.</div>
        <label style="display:flex;flex-direction:column;gap:4px;">Nazwa nowej instancji
          <tf-input id="dup-name"></tf-input>
        </label>
      </div>`,
    onConfirm: async (win) => {
      const name = (win.querySelector('#dup-name')?.value || '').trim();
      if (!name) { toast('Podaj nazwe instancji', 'error'); return false; }
      const res = await ApiBinary.action('addonInstanceDuplicateRequest', {
        sourceAddonId,
        newDisplayName: name,
      });
      if (!res.ok) { toast(res.error || 'Duplikacja nieudana', 'error'); return false; }
      toast(`Utworzono instancje "${name}"`, 'success');
      await refreshAll();
      return true;
    },
  });
}

// Instancja → "Odinstaluj": the shared dialog shows the teardown plan and
// asks for the instance name before the wipe.
function onUninstallInstance(addonId, displayName) {
  openUninstallDialog({ addonId, displayName, onDone: refreshAll });
}

// Po operacji na instancji/katalogu: przeladuj oba zrodla i przerenderuj widok.
async function refreshAll() {
  await Promise.all([loadList(), loadCatalog()]);
  renderActive();
}

async function onInstallZip() {
  // Okno dialogowe z file input oraz przyciskami Anuluj/Zainstaluj.
  const bodyHtml = `
    <div style="display:flex;flex-direction:column;gap:12px;font-size:13px;">
      <div style="color:var(--text-2);">${escapeHtml(I18n.t('addons.install_zip'))}</div>
      <input id="addons-zip-file" type="file" accept=".zip,.wasm" style="padding:10px;border:1px dashed var(--border);border-radius:8px;background:var(--bg-2);color:var(--text-2);" />
    </div>
  `;
  const footerHtml = `
    <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
    <tf-button variant="primary" icon="download" data-action="install">${escapeHtml(I18n.t('addons.install_zip'))}</tf-button>
  `;
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('addons.install_zip'));
  win.setAttribute('icon', 'download');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '420');
  win.setAttribute('width', '460');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const bodyWrap = document.createElement('div');
  bodyWrap.slot = 'body';
  bodyWrap.innerHTML = bodyHtml;
  const footWrap = document.createElement('div');
  footWrap.slot = 'footer';
  footWrap.innerHTML = footerHtml;
  win.appendChild(bodyWrap);
  win.appendChild(footWrap);

  win.addEventListener('action', async (e) => {
    if (e.detail?.action !== 'install') return;
    e.preventDefault();
    const input = win.querySelector('#addons-zip-file');
    const f = input?.files?.[0];
    if (!f) {
      toast(I18n.t('common.required'), 'error');
      return;
    }
    try {
      // Wczytaj plik jako Uint8Array — binary message niesie surowe bajty bez multipart parsowania.
      const buf = await f.arrayBuffer();
      const result = await ApiBinary.action('addonInstallRequest', {
        filename: f.name,
        content: new Uint8Array(buf),
      });
      if (!result.ok) {
        throw new Error(result.error || 'install_failed');
      }
      if (Array.isArray(result.warnings) && result.warnings.length > 0) {
        console.warn('[addons] install warnings:', result.warnings);
      }
      win.close(true);
      // Catalog-only upload: the package lands in the Catalog (a template to
      // install from), not the Installed list. Switch there and refresh.
      activeView = 'catalog';
      document
        .querySelectorAll('.addons-view-switch tf-chip[data-view]')
        .forEach((c) => c.toggleAttribute('active', c.dataset.view === 'catalog'));
      await loadCatalog();
      renderActive();
      toast(I18n.t('addons.uploaded_to_catalog'), 'success');
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });

  document.body.appendChild(win);
}

// --- Detail view -----------------------------------------------------------
function renderDetailSkeleton() {
  return `
    <div class="addon-detail">
      <div class="breadcrumb">
        <span class="crumb" id="crumb-addons-root">${escapeHtml(I18n.t('addons.list_title'))}</span>
        <svg class="icon icon-sm sep"><use href="#i-chevron-right"/></svg>
        <span class="crumb active" id="crumb-addon-current">${escapeHtml(I18n.t('common.loading'))}</span>
      </div>
      <div id="addon-detail-header-host"></div>
      <div id="addon-detail-tabs"></div>
      <div id="addon-detail-body" style="padding:4px 0;">
        <div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>
      </div>
    </div>
  `;
}

async function loadDetail() {
  try {
    currentAddonDetail = await ApiBinary.one('addonDetailRequest', { addonId: currentAddonId });
  } catch (err) {
    currentAddonDetail = null;
    const body = byId('addon-detail-body');
    if (body) body.innerHTML = `<div class="addons-empty" style="color:var(--danger);">${escapeHtml(err.message)}</div>`;
  }
}

function renderDetail() {
  if (!currentAddonDetail) return;
  const d = currentAddonDetail;

  const crumb = byId('crumb-addon-current');
  if (crumb) crumb.textContent = d.name || currentAddonId;
  byId('crumb-addons-root')?.addEventListener('click', () => backToList());

  renderDetailHeader(d);

  const tools = d.tools || [];
  const permissions = d.permissions || [];
  const linkedCount = Number(d.linkedAccountsCount ?? d.linked_accounts_count ?? 0);
  const visibleCount = Number(d.visibilityGroupsVisible ?? d.visibility_groups_visible ?? 0);
  const totalCount = Number(d.visibilityGroupsTotal ?? d.visibility_groups_total ?? 0);
  const oauthMode = d.oauthMode ?? d.oauth_mode ?? null;

  const tabsHost = byId('addon-detail-tabs');
  if (!tabsHost) return;

  const tabs = [
    { id: 'settings', icon: 'settings', label: I18n.t('addons.tab_settings'), adminOnly: true },
    {
      id: 'visibility',
      icon: 'users',
      label: I18n.t('addons.tab_visibility'),
      adminOnly: true,
      count: totalCount > 0 ? `${visibleCount}/${totalCount}` : null,
    },
    {
      id: 'permissions',
      icon: 'shield',
      label: I18n.t('addons.tab_permissions'),
      adminOnly: true,
      count: permissions.length > 0 ? String(permissions.length) : null,
    },
    {
      id: 'oauth',
      icon: 'key',
      label: I18n.t('addons.tab_oauth'),
      adminOnly: true,
      count: oauthMode ? oauthMode : null,
    },
    {
      id: 'linked',
      icon: 'user',
      label: I18n.t('addons.tab_linked_accounts'),
      adminOnly: true,
      count: linkedCount > 0 ? String(linkedCount) : null,
    },
    { id: 'access', icon: 'shield', label: I18n.t('addons.tab_access'), adminOnly: true },
    { id: 'bindings', icon: 'external-link', label: I18n.t('addons.tab_bindings'), adminOnly: true },
    { id: 'resources', icon: 'chip', label: I18n.t('addons.tab_resources'), adminOnly: true },
    { id: 'network', icon: 'globe', label: I18n.t('addons.tab_network'), adminOnly: true },
    { id: 'logs', icon: 'audit', label: I18n.t('addons.tab_logs'), adminOnly: true },
    {
      id: 'tools',
      icon: 'play',
      label: I18n.t('addons.tab_tools'),
      adminOnly: false,
      count: tools.length > 0 ? String(tools.length) : null,
    },
  ];
  const visible = tabs.filter((t) => {
    if (t.show === false) return false;
    if (t.adminOnly && !isAdmin) return false;
    return true;
  });

  tabsHost.innerHTML = `
    <tf-tabs variant="underline" value="${escapeAttr(activeTab)}" id="addon-tabs-nav">
      ${visible.map((t) => `
        <tf-tab id="${escapeAttr(t.id)}" icon="${escapeAttr(t.icon)}"${t.count ? ` count="${escapeAttr(t.count)}"` : ''}>${escapeHtml(t.label)}</tf-tab>
      `).join('')}
    </tf-tabs>
  `;
  tabsHost.querySelector('#addon-tabs-nav')?.addEventListener('change', (e) => {
    const id = e.detail?.value;
    if (id) switchTab(id);
  });

  switchTab(activeTab);
}

// Rendruje karte naglowka szczegolu addonu 1:1 z mockupem.
function renderDetailHeader(d) {
  const host = byId('addon-detail-header-host');
  if (!host) return;
  const iconId = d.icon || iconForCategory(d.category || '');
  const version = d.version || '0.0.0';
  const sizeBytes = Number(d.fileSizeBytes ?? d.file_size_bytes ?? 0);
  const sizeLabel = sizeBytes > 0 ? formatBytes(sizeBytes) : '';
  const runtime = d.runtime || '';
  const author = d.author || '';
  const license = d.license || '';

  const subParts = [`v${escapeHtml(version)}`];
  if (sizeLabel) subParts.push(escapeHtml(sizeLabel));
  if (runtime) subParts.push(escapeHtml(runtime));
  if (author) subParts.push(escapeHtml(I18n.t('addons.detail.by_author', { author })));
  if (license) subParts.push(escapeHtml(license));

  const enabled = !!(d.isEnabled ?? d.is_enabled);
  const oauthMode = d.oauthMode ?? d.oauth_mode ?? null;
  const visibleCount = Number(d.visibilityGroupsVisible ?? d.visibility_groups_visible ?? 0);
  const totalCount = Number(d.visibilityGroupsTotal ?? d.visibility_groups_total ?? 0);

  const badges = [];
  badges.push(renderAddonBadge(enabled ? I18n.t('addons.enabled') : I18n.t('addons.disabled'), {
    status: enabled ? 'ok' : 'warn',
    className: `state state-${enabled ? 'on' : 'off'}`,
  }));
  if (oauthMode) {
    const icon = oauthMode === 'global' ? 'globe' : oauthMode === 'individual' ? 'user' : oauthMode === 'mixed' ? 'share' : 'key';
    badges.push(renderAddonBadge(I18n.t('addons.badges.oauth_' + oauthMode), {
      status: oauthMode === 'global' ? 'ok' : oauthMode === 'none' ? 'warn' : 'accent',
      icon,
      className: `oauth-${oauthMode}`,
    }));
  }
  if (totalCount > 0) {
    badges.push(renderAddonBadge(I18n.t('addons.badges.visibility_n_of_m', { n: visibleCount, m: totalCount }), {
      status: 'info',
      icon: 'users',
      className: 'visibility',
    }));
  }
  // Native apps: per-node reconcile registry (plan §3.1) — one badge per
  // state so the admin sees at a glance which nodes are ready, unsupported
  // or failed; the failing node ids ride in the tooltip.
  const nodeStatuses = d.nodeStatuses ?? d.node_statuses ?? [];
  if (nodeStatuses.length > 0) {
    const byState = new Map();
    for (const s of nodeStatuses) {
      const key = s.status || 'unknown';
      if (!byState.has(key)) byState.set(key, []);
      byState.get(key).push(s);
    }
    for (const [stateKey, entries] of byState) {
      const statusKind = stateKey === 'ready' ? 'ok' : stateKey === 'unsupported' ? 'info' : 'warn';
      const label = `${I18n.t('addons.badges.node_status_' + stateKey)}: ${entries.length}`;
      const title = entries
        .map((s) => `${s.nodeId ?? s.node_id ?? ''}${s.detail ? ` — ${s.detail}` : ''}`)
        .join('\n');
      badges.push(`<span title="${escapeAttr(title)}">${renderAddonBadge(label, {
        status: statusKind,
        icon: 'server',
        className: `node-status node-status-${stateKey}`,
      })}</span>`);
    }
  }

  // Note: previously had "Konfiguruj" button which opened the install wizard
  // (F2-P7 6-step UI). Removed — that wizard is for fresh installs only, and
  // for an already-installed addon it duplicated functionality covered by
  // the per-addon tabs (Ustawienia / Uprawnienia / OAuth / Sieć / etc.).
  // The TentaVision-specific cameras step shouldn't appear for unrelated
  // addons either. Settings tab is the canonical configuration surface.
  const actions = isAdmin ? `
    <tf-button variant="ghost" icon="refresh" id="hdr-reload">${escapeHtml(I18n.t('addon_reload.button'))}</tf-button>
    <tf-button variant="danger" icon="trash" id="hdr-uninstall">${escapeHtml(I18n.t('addon_uninstall.button'))}</tf-button>
  ` : '';

  host.innerHTML = `
    <div class="detail-header">
      <div class="big-ico">${spriteRaw(iconId)}</div>
      <div class="d-meta">
        <div class="d-name">${escapeHtml(d.name || currentAddonId)}</div>
        <div class="d-sub">${subParts.join(' · ')}</div>
        <div class="d-badges">${badges.join('')}</div>
      </div>
      <div class="d-actions">${actions}</div>
    </div>
  `;
  host.querySelector('#hdr-reload')?.addEventListener('click', onReloadAddon);
  host.querySelector('#hdr-uninstall')?.addEventListener('click', onUninstallAddon);
}

async function switchTab(tabId) {
  // Tabs visible to non-admins. 'access' stays admin-only because its data
  // endpoint (addonAccessListRequest) is backend #[policy(Admin)]; a non-admin
  // read-only access endpoint is a future backend follow-up.
  const nonAdminTabs = new Set(['tools']);
  if (!isAdmin && !nonAdminTabs.has(tabId)) {
    tabId = 'tools';
  }
  activeTab = tabId;
  unmountActiveTab();
  const body = byId('addon-detail-body');
  if (!body) return;
  body.innerHTML = '';

  if (tabId === 'settings') {
    await SettingsTab.mount(body, currentAddonId);
    activeTabController = SettingsTab;
    return;
  }
  if (tabId === 'visibility') {
    await VisibilityTab.mount(body, currentAddonId, {
      adminOnlyInitial: !!(currentAddonDetail?.adminOnly ?? currentAddonDetail?.admin_only),
    });
    activeTabController = VisibilityTab;
    return;
  }
  if (tabId === 'permissions') {
    const addonName = currentAddonDetail?.name || currentAddonId;
    await PermissionsTab.mount(body, currentAddonId, addonName);
    activeTabController = PermissionsTab;
    return;
  }
  if (tabId === 'access') {
    const addonName = currentAddonDetail?.name || currentAddonId;
    await AccessTab.mount(body, currentAddonId, { addonName, isAdmin });
    activeTabController = AccessTab;
    return;
  }
  if (tabId === 'oauth') {
    const providers = currentAddonDetail?.oauthProviders || currentAddonDetail?.oauth_providers || [];
    if (providers.length === 0) {
      renderOAuthEmptyState(body);
      return;
    }
    await OAuthConfigTab.mount(body, currentAddonId, { providerDecls: providers });
    activeTabController = OAuthConfigTab;
    return;
  }
  if (tabId === 'linked') {
    const providers = currentAddonDetail?.oauthProviders || currentAddonDetail?.oauth_providers || [];
    if (providers.length === 0) {
      renderLinkedAccountsEmptyState(body);
      return;
    }
    await LinkedAccountsTab.mount(body, currentAddonId);
    activeTabController = LinkedAccountsTab;
    return;
  }
  if (tabId === 'bindings') {
    await BindingsTab.mount(body, currentAddonId);
    activeTabController = BindingsTab;
    return;
  }
  if (tabId === 'resources') {
    await ResourcesTab.mount(body, currentAddonId);
    activeTabController = ResourcesTab;
    return;
  }
  if (tabId === 'network') {
    await NetworkTab.mount(body, currentAddonId);
    activeTabController = NetworkTab;
    return;
  }
  if (tabId === 'logs') {
    await LogsTab.mount(body, currentAddonId);
    activeTabController = LogsTab;
    return;
  }
  if (tabId === 'tools') {
    await ToolsTab.mount(body, currentAddonId);
    activeTabController = ToolsTab;
    return;
  }
}

function unmountActiveTab() {
  if (activeTabController?.unmount) {
    try { activeTabController.unmount(); } catch {}
  }
  activeTabController = null;
}

// Empty state shown when addon declares no OAuth providers in its manifest.
function renderOAuthEmptyState(body) {
  const name = currentAddonDetail?.name || currentAddonId || '';
  const title = I18n.t('addons.oauth_empty_title');
  const sub = I18n.t('addons.oauth_empty_sub', { name }).replace('{name}', name);
  body.innerHTML = `
    <div class="empty-state">
      <svg><use href="#i-key"/></svg>
      <div class="empty-state-text">${escapeHtml(title)}</div>
      <div class="empty-state-sub">${escapeHtml(sub)}</div>
    </div>
  `;
}

// Empty state shown when addon declares no OAuth providers, hence no linked accounts possible.
function renderLinkedAccountsEmptyState(body) {
  const title = I18n.t('addons.linked_accounts_empty_title');
  const sub = I18n.t('addons.linked_accounts_empty_sub');
  body.innerHTML = `
    <div class="empty-state">
      <svg><use href="#i-user"/></svg>
      <div class="empty-state-text">${escapeHtml(title)}</div>
      <div class="empty-state-sub">${escapeHtml(sub)}</div>
    </div>
  `;
}

async function onReloadAddon() {
  try {
    await ApiBinary.action('addonReloadRequest', { addonId: currentAddonId });
    toast(I18n.t('addon_reload.success'), 'success');
  } catch (err) {
    toast(`${I18n.t('addon_reload.error')}: ${err.message}`, 'error');
  }
}

function onUninstallAddon() {
  openUninstallDialog({
    addonId: currentAddonId,
    displayName: currentAddonDetail?.displayName || currentAddonDetail?.name || currentAddonId,
    onDone: backToList,
  });
}

function backToList() {
  unmountActiveTab();
  currentAddonId = null;
  currentAddonDetail = null;
  const host = byId('main');
  if (!host) return;
  host.innerHTML = AddonsScreen.render();
  AddonsScreen.mount();
}

export default AddonsScreen;
