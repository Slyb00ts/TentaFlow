// ===== File: maps.js — Shared map: places, their reconstructions and devices =====
//
// P0 control surface over MessageBody::MapBody (binary protocol only, codec
// map* helpers). Two in-module views behind ONE route (`#/maps`, deep-linkable
// as `#/maps?site=<id>`):
//   SITES — every place the organization manages (`tf-table`: name, address,
//           scene count, devices online, last update) behind a `.tf-toolbar`.
//   SITE  — the scenes of one place plus the devices of the selected scene,
//           reached by a row click and left through the breadcrumb.
//
// Geometry never passes through here: the 3D view, placement editing, drift
// correction and the `map:` stream are P2/P3 (plan §8 pt. 2-3), so this screen
// deliberately shows counts and state, never a rendering.
//
// Permission gating reads `my_permissions` off the site list (plan §9): the
// server computes `map.read` / `map.write` / `map.admin` from the caller's RBAC
// permissions, so the screen never infers them from a role. Before that reply
// lands nothing mutating is drawn — a guessed button is a button the server
// will refuse. The server stays the real gate; a refused mutation surfaces its
// own message.
// tf-* components only; every visible string comes from i18n maps.*.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { Router } from '/js/router.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-select.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-table.js';
import '/js/components/tf-window.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-empty-state.js';

// Device session states the owner node publishes; anything else is shown raw so
// a newer server's state is visible rather than silently relabelled.
const DEVICE_STATE_CHIP = {
  placed: 'ok',
  relocalizing: 'warn',
  unplaced: 'info',
  lost: 'err',
};

const PLACEMENT_METHODS = ['identity', 'manual', 'icp'];

// Default cell resolution of a new scene, matching `map_scenes.voxel_res_m`.
const DEFAULT_VOXEL_RES_M = 0.05;

// `write`/`admin` start false and only ever come from a site-list reply. There
// is no `read` flag: the reply itself is the proof of `map.read`, because the
// handler refuses the request without it.
const state = {
  perms: { write: false, admin: false },
  sites: [],
  siteId: null,
  scenes: [],
  sceneId: null,
  devices: [],
  nodes: [],
  query: '',
  wins: new Set(),
};

function t(key, params) {
  return I18n.t(`maps.${key}`, params);
}

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

// The wasm decoder emits both spellings of every wire field; a row read through
// one spelling only would be empty for half the payloads.
function fv(obj, snake) {
  if (!obj) return undefined;
  if (obj[snake] !== undefined) return obj[snake];
  const camel = snake.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
  return obj[camel];
}

function formatMs(ms) {
  const n = Number(ms ?? 0);
  if (!Number.isFinite(n) || n <= 0) return '—';
  return new Date(n).toLocaleString(I18n.getLanguage());
}

function formatNum(n) {
  const v = Number(n ?? 0);
  if (!Number.isFinite(v)) return '—';
  return v.toLocaleString(I18n.getLanguage());
}

function shortId(id) {
  const s = String(id ?? '');
  return s.length > 12 ? `${s.slice(0, 12)}…` : s;
}

function localNodeId() {
  const local = state.nodes.find((n) => fv(n, 'is_local') || n.source === 'local');
  return String(fv(local, 'node_id') ?? '');
}

function nodeLabel(nodeId) {
  const id = String(nodeId ?? '');
  if (!id) return t('owner_none');
  const node = state.nodes.find((n) => String(fv(n, 'node_id') ?? '') === id);
  const host = String(node?.hostname ?? '').trim();
  return host || shortId(id);
}

/** The server's own message, or a generic one when it sent none. */
function errText(resp) {
  const raw = fv(resp, 'error');
  const msg = typeof raw === 'string' ? raw.trim() : '';
  return msg || t('error_generic');
}

// =============================================================================
// Screen
// =============================================================================

const MapsScreen = {
  get title() { return t('title'); },

  render() {
    return `
      <div id="maps-root">
        <div id="maps-sites-view">
          <div class="page-header">
            <div>
              <h1>${sprite('pin')} ${escapeHtml(t('title'))}</h1>
              <div class="sub">${escapeHtml(t('subtitle'))}</div>
            </div>
          </div>
          <div class="tf-toolbar" id="maps-toolbar"></div>
          <div id="maps-sites-host"><div class="maps-loading">${escapeHtml(t('loading'))}</div></div>
        </div>
        <div id="maps-site-view" hidden></div>
      </div>
    `;
  },

  async mount(params = {}) {
    // The mesh node list only decorates owner ids with hostnames and marks the
    // local node; a node list we cannot read must not hide the map.
    state.nodes = await ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).catch(() => []);

    await loadSites();

    const site = String(params.site ?? '');
    if (site && state.sites.some((s) => String(fv(s, 'site_id')) === site)) {
      await openSite(site, { pushUrl: false });
    }
  },

  unmount() {
    closeAllWindows();
    state.perms = { write: false, admin: false };
    state.sites = [];
    state.scenes = [];
    state.devices = [];
    state.nodes = [];
    state.siteId = null;
    state.sceneId = null;
    state.query = '';
  },
};

export default MapsScreen;

// =============================================================================
// Windows
// =============================================================================

function closeAllWindows() {
  for (const cleanup of [...state.wins]) {
    try { cleanup(); } catch { /* window already gone */ }
  }
  state.wins.clear();
}

function openWindow({ title, subtitle, icon = 'pin', width = 520 }) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', title);
  if (subtitle) win.setAttribute('subtitle', subtitle);
  win.setAttribute('icon', icon);
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', String(width));
  win.setAttribute('draggable', '');

  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'maps-window-body';
  win.appendChild(body);

  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.className = 'maps-window-footer';
  win.appendChild(foot);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);

  const cleanup = () => {
    if (win.isConnected) win.close(true);
    if (backdrop.isConnected) backdrop.remove();
    state.wins.delete(cleanup);
  };
  state.wins.add(cleanup);
  win.addEventListener('close-request', () => {
    if (backdrop.isConnected) backdrop.remove();
    state.wins.delete(cleanup);
  });
  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'close') cleanup();
  });

  return { win, body, foot, cleanup };
}

function footerHtml(confirmLabel, { danger = false, action = 'confirm' } = {}) {
  return `
    <div class="maps-footer-right">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(t('action_cancel'))}</tf-button>
      <tf-button variant="${danger ? 'danger-solid' : 'primary'}" data-action="${action}">${escapeHtml(confirmLabel)}</tf-button>
    </div>
  `;
}

function fieldValue(body, id) {
  return String(body.querySelector(`#${id}`)?.value ?? '').trim();
}

/** Trimmed numeric field, or null when the operator left it empty. */
function numberField(body, id) {
  const raw = fieldValue(body, id);
  if (!raw) return null;
  const n = Number(raw.replace(',', '.'));
  return Number.isFinite(n) ? n : null;
}

// =============================================================================
// Sites
// =============================================================================

// Rebuilt on every site-list reload, because the create button depends on
// permissions that arrive with it — hence the searchbox carries `state.query`
// back, or a refresh would blank the field while the filter stayed applied.
function renderToolbar() {
  const bar = byId('maps-toolbar');
  if (!bar) return;
  bar.innerHTML = `
    <tf-searchbox id="maps-search" placeholder="${escapeAttr(t('search_placeholder'))}" value="${escapeAttr(state.query)}" debounce="200"></tf-searchbox>
    <tf-button variant="ghost" icon="refresh" id="maps-refresh">${escapeHtml(t('refresh'))}</tf-button>
    <span class="tf-toolbar-spacer"></span>
    ${state.perms.admin
      ? `<tf-button variant="primary" icon="plus" id="maps-new">${escapeHtml(t('new_site'))}</tf-button>`
      : ''}
  `;
  byId('maps-search')?.addEventListener('search', (e) => {
    state.query = String(e.detail?.value ?? '');
    renderSites();
  });
  byId('maps-refresh')?.addEventListener('click', () => loadSites());
  byId('maps-new')?.addEventListener('click', () => openSiteWindow(null));
}

// Read as a whole body, not through `list`: the same reply carries the caller's
// effective map permissions, and the toolbar has to be redrawn from them.
async function loadSites() {
  try {
    const body = await ApiBinary.one('mapSiteListRequest');
    const granted = new Set((fv(body, 'my_permissions') ?? []).map((p) => String(p)));
    state.perms = { write: granted.has('map.write'), admin: granted.has('map.admin') };
    state.sites = fv(body, 'sites') ?? [];
  } catch (e) {
    // A failed read tells us nothing about what the caller may change, so the
    // mutation surfaces go back to hidden rather than staying from last time.
    state.perms = { write: false, admin: false };
    state.sites = [];
    toast(e.message || t('error_generic'), 'error');
  }
  renderToolbar();
  renderSites();
}

function visibleSites() {
  const q = state.query.trim().toLowerCase();
  if (!q) return state.sites;
  return state.sites.filter((s) => {
    const hay = `${fv(s, 'name') ?? ''} ${fv(s, 'address') ?? ''} ${fv(s, 'description') ?? ''}`;
    return hay.toLowerCase().includes(q);
  });
}

function renderSites() {
  const host = byId('maps-sites-host');
  if (!host) return;
  const rows = visibleSites();

  if (rows.length === 0) {
    // An administrator sees how to fill the screen; anyone else is told who can.
    host.innerHTML = `
      <tf-empty-state
        icon="pin"
        title="${escapeAttr(state.sites.length === 0 ? t('empty_title') : t('empty_filtered_title'))}"
        message="${escapeAttr(state.sites.length === 0
          ? (state.perms.admin ? t('empty_admin_message') : t('empty_viewer_message'))
          : t('empty_filtered_message'))}">
        ${state.sites.length === 0 && state.perms.admin
          ? `<tf-button variant="primary" icon="plus" id="maps-empty-new">${escapeHtml(t('new_site'))}</tf-button>`
          : ''}
      </tf-empty-state>
    `;
    byId('maps-empty-new')?.addEventListener('click', () => openSiteWindow(null));
    return;
  }

  host.innerHTML = `
    <tf-table id="maps-sites-table">
      <tf-column key="name" label="${escapeAttr(t('col_site'))}" renderer="html" fill></tf-column>
      <tf-column key="address" label="${escapeAttr(t('col_address'))}"></tf-column>
      <tf-column key="scenes" label="${escapeAttr(t('col_scenes'))}" renderer="num"></tf-column>
      <tf-column key="devices" label="${escapeAttr(t('col_devices_online'))}" renderer="num"></tf-column>
      <tf-column key="updated" label="${escapeAttr(t('col_last_update'))}"></tf-column>
    </tf-table>
    <div class="maps-table-footer">${escapeHtml(t('summary_sites', { count: rows.length }))}</div>
  `;
  const table = byId('maps-sites-table');
  table.rows = rows.map((s) => ({
    _id: String(fv(s, 'site_id') ?? ''),
    name: `<div class="tf-table__cell-title">${escapeHtml(String(fv(s, 'name') ?? ''))}</div>`
      + `<div class="tf-table__cell-sub">${escapeHtml(String(fv(s, 'description') ?? ''))}</div>`,
    address: String(fv(s, 'address') ?? '') || '—',
    scenes: Number(fv(s, 'scene_count') ?? 0),
    devices: Number(fv(s, 'devices_online') ?? 0),
    updated: formatMs(fv(s, 'last_update_ms') ?? fv(s, 'updated_at_ms')),
  }));
  if (state.perms.admin) {
    table.rowActions = (row, _idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      return actionRow([
        { label: t('action_edit'), variant: 'ghost', run: () => openSiteWindow(siteById(live()._id)) },
        { label: t('action_delete'), variant: 'danger', run: () => openSiteDeleteWindow(siteById(live()._id)) },
      ]);
    };
  }
  table.addEventListener('row-click', (e) => {
    const id = e.detail?.row?._id;
    if (id) openSite(id);
  });
}

/// Row-action buttons are live elements: a table cell lives in tf-table's shadow
/// root, where event retargeting hides the clicked button from a host listener.
function actionRow(actions) {
  const wrap = document.createElement('div');
  wrap.className = 'tf-table__actions';
  for (const a of actions) {
    const btn = document.createElement('tf-button');
    btn.setAttribute('variant', a.variant || 'ghost');
    btn.setAttribute('size', 'sm');
    btn.textContent = a.label;
    btn.addEventListener('click', a.run);
    wrap.appendChild(btn);
  }
  return wrap;
}

function siteById(id) {
  return state.sites.find((s) => String(fv(s, 'site_id')) === String(id)) ?? null;
}

function sceneById(id) {
  return state.scenes.find((s) => String(fv(s, 'scene_id')) === String(id)) ?? null;
}

function openSiteWindow(site) {
  const editing = !!site;
  const { body, foot, cleanup } = openWindow({
    title: editing ? t('site_edit_title') : t('site_new_title'),
    icon: 'pin',
  });
  body.innerHTML = `
    <tf-input id="maps-site-name" label="${escapeAttr(t('field_name'))}" value="${escapeAttr(String(fv(site, 'name') ?? ''))}"></tf-input>
    <tf-textarea id="maps-site-desc" label="${escapeAttr(t('field_description'))}" rows="3">${escapeHtml(String(fv(site, 'description') ?? ''))}</tf-textarea>
    <tf-input id="maps-site-address" label="${escapeAttr(t('field_address'))}" value="${escapeAttr(String(fv(site, 'address') ?? ''))}"></tf-input>
    <div class="maps-field-row">
      <tf-input id="maps-site-lat" label="${escapeAttr(t('field_lat'))}" value="${escapeAttr(numText(fv(site, 'lat')))}"></tf-input>
      <tf-input id="maps-site-lon" label="${escapeAttr(t('field_lon'))}" value="${escapeAttr(numText(fv(site, 'lon')))}"></tf-input>
      <tf-input id="maps-site-alt" label="${escapeAttr(t('field_alt'))}" value="${escapeAttr(numText(fv(site, 'alt')))}"></tf-input>
    </div>
  `;
  foot.innerHTML = footerHtml(t('action_save'));
  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    const name = fieldValue(body, 'maps-site-name');
    if (!name) { toast(t('error_name_required'), 'warn'); return; }
    const resp = await ApiBinary.action('mapSiteUpsertRequest', {
      site: {
        siteId: editing ? String(fv(site, 'site_id')) : '',
        name,
        description: fieldValue(body, 'maps-site-desc'),
        address: fieldValue(body, 'maps-site-address'),
        lat: numberField(body, 'maps-site-lat'),
        lon: numberField(body, 'maps-site-lon'),
        alt: numberField(body, 'maps-site-alt'),
      },
    }).catch((err) => ({ ok: false, error: err.message }));
    if (!fv(resp, 'ok')) { toast(errText(resp), 'error'); return; }
    cleanup();
    toast(t('toast_site_saved'), 'success');
    await loadSites();
  });
}

function numText(v) {
  return v === null || v === undefined ? '' : String(v);
}

function openSiteDeleteWindow(site) {
  if (!site) return;
  const { body, foot, cleanup } = openWindow({
    title: t('delete_site_title'),
    subtitle: String(fv(site, 'name') ?? ''),
    icon: 'alert',
    width: 480,
  });
  body.innerHTML = `
    <div class="maps-danger">${sprite('alert')}<span>${escapeHtml(t('delete_site_warning'))}</span></div>
    <div class="maps-error" id="maps-site-del-error" hidden></div>
  `;
  foot.innerHTML = footerHtml(t('action_delete_confirm'), { danger: true });
  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    const resp = await ApiBinary.action('mapSiteDeleteRequest', { siteId: String(fv(site, 'site_id')) })
      .catch((err) => ({ ok: false, error: err.message }));
    if (!fv(resp, 'ok')) {
      const box = body.querySelector('#maps-site-del-error');
      box.textContent = errText(resp);
      box.hidden = false;
      return;
    }
    cleanup();
    toast(t('toast_site_deleted'), 'success');
    await loadSites();
  });
}

// =============================================================================
// One site: scenes + devices
// =============================================================================

async function openSite(siteId, { pushUrl = true } = {}) {
  state.siteId = String(siteId);
  state.sceneId = null;
  state.devices = [];
  byId('maps-sites-view')?.setAttribute('hidden', '');
  const view = byId('maps-site-view');
  if (view) view.hidden = false;
  if (pushUrl) Router.replaceParams({ site: state.siteId });
  await loadScenes();
}

function backToSites() {
  state.siteId = null;
  state.sceneId = null;
  state.scenes = [];
  state.devices = [];
  const view = byId('maps-site-view');
  if (view) { view.hidden = true; view.innerHTML = ''; }
  byId('maps-sites-view')?.removeAttribute('hidden');
  Router.replaceParams({});
  renderSites();
}

async function loadScenes() {
  try {
    state.scenes = await ApiBinary.list('mapSceneListRequest', {
      arrayKey: 'scenes',
      payload: { siteId: state.siteId },
    });
  } catch (e) {
    state.scenes = [];
    toast(e.message || t('error_generic'), 'error');
  }
  // Keep the selected scene only while it still exists, otherwise fall back to
  // the first one so the devices panel is never bound to a deleted scene.
  if (!state.sceneId || !sceneById(state.sceneId)) {
    state.sceneId = state.scenes.length ? String(fv(state.scenes[0], 'scene_id')) : null;
  }
  renderSiteView();
  if (state.sceneId) await loadDevices();
}

async function loadDevices() {
  if (!state.sceneId) { state.devices = []; renderDevices(); return; }
  try {
    state.devices = await ApiBinary.list('mapDeviceListRequest', {
      arrayKey: 'devices',
      payload: { sceneId: state.sceneId },
    });
  } catch (e) {
    state.devices = [];
    toast(e.message || t('error_generic'), 'error');
  }
  renderDevices();
}

function renderSiteView() {
  const view = byId('maps-site-view');
  if (!view) return;
  const site = siteById(state.siteId);
  const name = String(fv(site, 'name') ?? '');

  view.innerHTML = `
    <tf-breadcrumb id="maps-crumbs">
      <tf-breadcrumb-item href="#/maps">${escapeHtml(t('title'))}</tf-breadcrumb-item>
      <tf-breadcrumb-item current>${escapeHtml(name)}</tf-breadcrumb-item>
    </tf-breadcrumb>
    <div class="page-header">
      <div>
        <h1>${sprite('pin')} ${escapeHtml(name)}</h1>
        <div class="sub">${escapeHtml(String(fv(site, 'address') ?? '') || t('site_no_address'))}</div>
      </div>
    </div>
    <div class="tf-toolbar">
      <tf-button variant="ghost" icon="arrow-left" id="maps-back">${escapeHtml(t('back_to_sites'))}</tf-button>
      <span class="tf-toolbar-spacer"></span>
      ${state.perms.admin
        ? `<tf-button variant="primary" icon="plus" id="maps-new-scene">${escapeHtml(t('new_scene'))}</tf-button>`
        : ''}
    </div>
    <div id="maps-scenes-host"></div>
    <div id="maps-devices-host"></div>
  `;

  // The breadcrumb renders a real <a>; handling it here keeps the search filter
  // instead of letting a hashchange remount the whole screen.
  byId('maps-crumbs')?.addEventListener('click', (e) => {
    const link = e.target.closest('a');
    if (!link) return;
    e.preventDefault();
    backToSites();
  });
  byId('maps-back')?.addEventListener('click', () => backToSites());
  byId('maps-new-scene')?.addEventListener('click', () => openSceneWindow(null));

  renderScenes();
  renderDevices();
}

function renderScenes() {
  const host = byId('maps-scenes-host');
  if (!host) return;
  if (state.scenes.length === 0) {
    host.innerHTML = `
      <tf-empty-state icon="layers" title="${escapeAttr(t('scenes_empty_title'))}"
        message="${escapeAttr(state.perms.admin ? t('scenes_empty_admin_message') : t('scenes_empty_viewer_message'))}">
      </tf-empty-state>
    `;
    return;
  }

  host.innerHTML = `
    <h2 class="maps-section-title">${escapeHtml(t('scenes_title'))}</h2>
    <tf-table id="maps-scenes-table">
      <tf-column key="name" label="${escapeAttr(t('col_scene'))}" renderer="html" fill></tf-column>
      <tf-column key="owner" label="${escapeAttr(t('col_owner'))}" renderer="html"></tf-column>
      <tf-column key="res" label="${escapeAttr(t('col_resolution'))}"></tf-column>
      <tf-column key="geometry" label="${escapeAttr(t('col_geometry'))}"></tf-column>
      <tf-column key="geo" label="${escapeAttr(t('col_geo'))}" renderer="chip"></tf-column>
      <tf-column key="updated" label="${escapeAttr(t('col_last_update'))}"></tf-column>
    </tf-table>
    <div class="maps-table-footer">${escapeHtml(t('summary_scenes', { count: state.scenes.length }))}</div>
  `;
  const table = byId('maps-scenes-table');
  const local = localNodeId();
  table.rows = state.scenes.map((s) => {
    const ownerId = String(fv(s, 'owner_node_id') ?? '');
    const isLocal = !!ownerId && ownerId === local;
    const online = !!fv(s, 'owner_online');
    const hasGeo = fv(s, 'geo_lat') !== null && fv(s, 'geo_lat') !== undefined;
    const sceneId = String(fv(s, 'scene_id') ?? '');
    return {
      _id: sceneId,
      _class: sceneId === state.sceneId ? 'selected' : '',
      name: `<div class="tf-table__cell-title">${escapeHtml(String(fv(s, 'name') ?? ''))}</div>`
        + `<div class="tf-table__cell-sub">${escapeHtml(shortId(sceneId))}</div>`,
      owner: `<div class="tf-table__cell-title">${escapeHtml(nodeLabel(ownerId))}</div>`
        + `<div class="tf-table__cell-sub">${escapeHtml(
          `${isLocal ? t('owner_this_node') : t('owner_remote_node')} · ${online ? t('owner_online') : t('owner_offline')}`,
        )}</div>`,
      res: t('resolution_value', { m: fv(s, 'voxel_res_m') ?? DEFAULT_VOXEL_RES_M }),
      geometry: t('geometry_value', {
        chunks: formatNum(fv(s, 'chunks')),
        voxels: formatNum(fv(s, 'voxels')),
      }),
      geo: hasGeo ? { status: 'ok', label: t('geo_set') } : { status: 'info', label: t('geo_missing') },
      updated: formatMs(fv(s, 'last_update_ms') ?? fv(s, 'updated_at_ms')),
    };
  });
  if (state.perms.admin) {
    table.rowActions = (row, _idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      return actionRow([
        { label: t('action_edit'), variant: 'ghost', run: () => openSceneWindow(sceneById(live()._id)) },
        { label: t('action_owner'), variant: 'ghost', run: () => openOwnerWindow(sceneById(live()._id)) },
        { label: t('action_geo'), variant: 'ghost', run: () => openGeoWindow(sceneById(live()._id)) },
        { label: t('action_delete'), variant: 'danger', run: () => openSceneDeleteWindow(sceneById(live()._id)) },
      ]);
    };
  }
  table.addEventListener('row-click', async (e) => {
    const id = e.detail?.row?._id;
    if (!id || id === state.sceneId) return;
    state.sceneId = String(id);
    renderScenes();
    await loadDevices();
  });
}

function openSceneWindow(scene) {
  const editing = !!scene;
  const { body, foot, cleanup } = openWindow({
    title: editing ? t('scene_edit_title') : t('scene_new_title'),
    icon: 'layers',
  });
  const ownerId = editing ? String(fv(scene, 'owner_node_id') ?? '') : localNodeId();
  body.innerHTML = `
    <tf-input id="maps-scene-name" label="${escapeAttr(t('field_name'))}" value="${escapeAttr(String(fv(scene, 'name') ?? ''))}"></tf-input>
    <div class="maps-field-row">
      <tf-input id="maps-scene-res" label="${escapeAttr(t('field_voxel_res'))}" value="${escapeAttr(String(fv(scene, 'voxel_res_m') ?? DEFAULT_VOXEL_RES_M))}"></tf-input>
      <tf-input id="maps-scene-max" label="${escapeAttr(t('field_max_voxels'))}" value="${escapeAttr(String(fv(scene, 'max_voxels') ?? 0))}"></tf-input>
    </div>
    ${nodeSelectHtml('maps-scene-owner', ownerId)}
  `;
  foot.innerHTML = footerHtml(t('action_save'));
  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    const name = fieldValue(body, 'maps-scene-name');
    if (!name) { toast(t('error_name_required'), 'warn'); return; }
    const resp = await ApiBinary.action('mapSceneUpsertRequest', {
      scene: {
        sceneId: editing ? String(fv(scene, 'scene_id')) : '',
        siteId: state.siteId,
        name,
        voxelResM: numberField(body, 'maps-scene-res') ?? DEFAULT_VOXEL_RES_M,
        ownerNodeId: fieldValue(body, 'maps-scene-owner'),
        maxVoxels: numberField(body, 'maps-scene-max') ?? 0,
        // Georeference has its own action: editing a scene must not silently
        // clear an anchor the operator set from the map.
        geoLat: fv(scene, 'geo_lat') ?? null,
        geoLon: fv(scene, 'geo_lon') ?? null,
        geoAlt: fv(scene, 'geo_alt') ?? null,
        geoHeading: fv(scene, 'geo_heading') ?? null,
      },
    }).catch((err) => ({ ok: false, error: err.message }));
    if (!fv(resp, 'ok')) { toast(errText(resp), 'error'); return; }
    cleanup();
    toast(t('toast_scene_saved'), 'success');
    await loadScenes();
    await loadSites();
  });
}

function nodeSelectHtml(id, selected) {
  const options = state.nodes.map((n) => {
    const nid = String(fv(n, 'node_id') ?? '');
    const label = `${nodeLabel(nid)}${fv(n, 'is_local') || n.source === 'local' ? ` · ${t('owner_this_node')}` : ''}`;
    return `<option value="${escapeAttr(nid)}"${nid === selected ? ' selected' : ''}>${escapeHtml(label)}</option>`;
  }).join('');
  // A node the mesh list does not carry (an owner that went away) still has to
  // stay selectable as the current value, so it is added explicitly.
  const known = state.nodes.some((n) => String(fv(n, 'node_id') ?? '') === selected);
  const extra = selected && !known
    ? `<option value="${escapeAttr(selected)}" selected>${escapeHtml(shortId(selected))}</option>`
    : '';
  return `<tf-select id="${id}" label="${escapeAttr(t('field_owner_node'))}">${extra}${options}</tf-select>`;
}

function openOwnerWindow(scene) {
  if (!scene) return;
  const { body, foot, cleanup } = openWindow({
    title: t('owner_title'),
    subtitle: String(fv(scene, 'name') ?? ''),
    icon: 'network',
  });
  body.innerHTML = `
    <div class="maps-danger">${sprite('alert')}<span>${escapeHtml(t('owner_warning'))}</span></div>
    ${nodeSelectHtml('maps-owner-node', String(fv(scene, 'owner_node_id') ?? ''))}
    <div class="maps-error" id="maps-owner-error" hidden></div>
  `;
  foot.innerHTML = footerHtml(t('action_save'));
  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    const nodeId = fieldValue(body, 'maps-owner-node');
    if (!nodeId) { toast(t('error_node_required'), 'warn'); return; }
    // `acceptLoss` stays false here: handing a scene over while the current
    // owner is unreachable means losing what it has not replicated, and P0 has
    // no window that states that loss — the server refusal is surfaced instead.
    const resp = await ApiBinary.action('mapSceneSetOwnerRequest', {
      sceneId: String(fv(scene, 'scene_id')),
      nodeId,
      acceptLoss: false,
    }).catch((err) => ({ ok: false, error: err.message }));
    if (!fv(resp, 'ok')) {
      const box = body.querySelector('#maps-owner-error');
      box.textContent = errText(resp);
      box.hidden = false;
      return;
    }
    cleanup();
    toast(t('toast_owner_set'), 'success');
    await loadScenes();
  });
}

function openGeoWindow(scene) {
  if (!scene) return;
  const { body, foot, cleanup } = openWindow({
    title: t('geo_title'),
    subtitle: String(fv(scene, 'name') ?? ''),
    icon: 'globe',
  });
  body.innerHTML = `
    <div class="maps-hint">${escapeHtml(t('geo_hint'))}</div>
    <div class="maps-field-row">
      <tf-input id="maps-geo-lat" label="${escapeAttr(t('field_lat'))}" value="${escapeAttr(numText(fv(scene, 'geo_lat')))}"></tf-input>
      <tf-input id="maps-geo-lon" label="${escapeAttr(t('field_lon'))}" value="${escapeAttr(numText(fv(scene, 'geo_lon')))}"></tf-input>
    </div>
    <div class="maps-field-row">
      <tf-input id="maps-geo-alt" label="${escapeAttr(t('field_alt'))}" value="${escapeAttr(numText(fv(scene, 'geo_alt')))}"></tf-input>
      <tf-input id="maps-geo-heading" label="${escapeAttr(t('field_heading'))}" value="${escapeAttr(numText(fv(scene, 'geo_heading')))}"></tf-input>
    </div>
    <div class="maps-error" id="maps-geo-error" hidden></div>
  `;
  foot.innerHTML = footerHtml(t('action_save'));
  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    const resp = await ApiBinary.action('mapSceneGeoAnchorSetRequest', {
      sceneId: String(fv(scene, 'scene_id')),
      lat: numberField(body, 'maps-geo-lat'),
      lon: numberField(body, 'maps-geo-lon'),
      alt: numberField(body, 'maps-geo-alt'),
      heading: numberField(body, 'maps-geo-heading'),
    }).catch((err) => ({ ok: false, error: err.message }));
    if (!fv(resp, 'ok')) {
      const box = body.querySelector('#maps-geo-error');
      box.textContent = errText(resp);
      box.hidden = false;
      return;
    }
    cleanup();
    toast(t('toast_geo_set'), 'success');
    await loadScenes();
  });
}

/// Deleting a scene is a two-answer decision: the first confirmation asks to
/// delete it, and `force` is sent ONLY after the server refused because geometry
/// exists AND the operator confirmed that second, harder question. The refusal
/// text comes from the server — retrying silently would destroy the map the
/// server just protected.
function openSceneDeleteWindow(scene) {
  if (!scene) return;
  const { body, foot, cleanup } = openWindow({
    title: t('delete_scene_title'),
    subtitle: String(fv(scene, 'name') ?? ''),
    icon: 'alert',
    width: 520,
  });
  body.innerHTML = `
    <div class="maps-danger">${sprite('alert')}<span>${escapeHtml(t('delete_scene_warning'))}</span></div>
    <div class="maps-error" id="maps-scene-del-error" hidden></div>
    <div class="maps-danger" id="maps-scene-del-force-note" hidden>${sprite('alert')}<span>${escapeHtml(t('delete_scene_force_warning'))}</span></div>
  `;
  foot.innerHTML = footerHtml(t('action_delete_confirm'), { danger: true });

  const send = async (force) => {
    const resp = await ApiBinary.action('mapSceneDeleteRequest', {
      sceneId: String(fv(scene, 'scene_id')),
      force,
    }).catch((err) => ({ ok: false, error: err.message }));
    if (fv(resp, 'ok')) {
      cleanup();
      toast(t('toast_scene_deleted'), 'success');
      await loadScenes();
      await loadSites();
      return;
    }
    const box = body.querySelector('#maps-scene-del-error');
    box.textContent = errText(resp);
    box.hidden = false;
    if (force) return;
    // The refusal was about geometry the operator can still choose to drop.
    body.querySelector('#maps-scene-del-force-note').hidden = false;
    const btn = foot.querySelector('[data-action="confirm"]');
    btn.dataset.action = 'force';
    btn.textContent = t('action_delete_force');
  };

  foot.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    send(btn.dataset.action === 'force');
  });
}

// =============================================================================
// Devices of the selected scene
// =============================================================================

function renderDevices() {
  const host = byId('maps-devices-host');
  if (!host) return;
  if (!state.sceneId) { host.innerHTML = ''; return; }
  const scene = sceneById(state.sceneId);
  const placed = state.devices;

  host.innerHTML = `
    <div class="tf-toolbar">
      <h2 class="maps-section-title">${escapeHtml(t('devices_title', { scene: String(fv(scene, 'name') ?? '') }))}</h2>
      <span class="tf-toolbar-spacer"></span>
      ${state.perms.write
        ? `<tf-button variant="primary" icon="plus" id="maps-assign">${escapeHtml(t('action_assign'))}</tf-button>`
        : ''}
    </div>
    ${placed.length === 0
      ? `<tf-empty-state icon="cpu" title="${escapeAttr(t('devices_empty_title'))}" message="${escapeAttr(t('devices_empty_message'))}"></tf-empty-state>`
      : `
        <tf-table id="maps-devices-table">
          <tf-column key="device" label="${escapeAttr(t('col_device'))}" renderer="html" fill></tf-column>
          <tf-column key="state" label="${escapeAttr(t('col_state'))}" renderer="chip"></tf-column>
          <tf-column key="method" label="${escapeAttr(t('col_method'))}"></tf-column>
          <tf-column key="drift" label="${escapeAttr(t('col_drift'))}"></tf-column>
          <tf-column key="frame" label="${escapeAttr(t('col_last_frame'))}"></tf-column>
        </tf-table>
        <div class="maps-table-footer">${escapeHtml(t('summary_devices', { count: placed.length }))}</div>
      `}
  `;
  byId('maps-assign')?.addEventListener('click', () => openAssignWindow());

  const table = byId('maps-devices-table');
  if (!table) return;
  table.rows = placed.map((d) => {
    const nodeId = String(fv(d, 'node_id') ?? '');
    const deviceId = String(fv(d, 'device_id') ?? '');
    const st = String(fv(d, 'state') ?? '');
    const placement = fv(d, 'placement');
    const method = String(fv(placement, 'method') ?? '');
    const drift = fv(d, 'drift_estimate_m');
    return {
      _id: `${nodeId}|${deviceId}`,
      device: `<div class="tf-table__cell-title">${escapeHtml(String(fv(d, 'display_name') ?? '') || deviceId)}</div>`
        + `<div class="tf-table__cell-sub">${escapeHtml(`${nodeLabel(nodeId)} · ${deviceId}`)}</div>`,
      state: { status: DEVICE_STATE_CHIP[st] ?? 'info', label: deviceStateLabel(st) },
      method: method ? methodLabel(method) : '—',
      drift: drift === null || drift === undefined ? '—' : t('drift_value', { m: Number(drift).toFixed(2) }),
      frame: formatMs(fv(d, 'last_frame_ms')),
    };
  });
  if (state.perms.write) {
    table.rowActions = (row, _idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      return actionRow([
        { label: t('action_unassign'), variant: 'danger', run: () => unassignDevice(live()._id) },
      ]);
    };
  }
}

function deviceStateLabel(st) {
  return DEVICE_STATE_CHIP[st] ? t(`device_state_${st}`) : (st || '—');
}

function methodLabel(method) {
  return PLACEMENT_METHODS.includes(method) ? t(`method_${method}`) : method;
}

async function unassignDevice(key) {
  const [nodeId, deviceId] = String(key).split('|');
  const resp = await ApiBinary.action('mapDeviceUnassignRequest', { nodeId, deviceId })
    .catch((err) => ({ ok: false, error: err.message }));
  if (!fv(resp, 'ok')) { toast(errText(resp), 'error'); return; }
  toast(t('toast_device_unassigned'), 'success');
  await loadDevices();
  await loadSites();
}

// Opened from the assign button only. The unfiltered device list below is an
// ORG-WIDE query — it must stay on this one-per-click path and never move into
// a render, where every scene selection and every reload would re-run it.
async function openAssignWindow() {
  const { body, foot, cleanup } = openWindow({ title: t('assign_title'), icon: 'cpu' });
  body.innerHTML = `<div class="maps-hint">${escapeHtml(t('loading'))}</div>`;
  foot.innerHTML = footerHtml(t('action_assign'));

  // A device already bound to THIS scene is not a candidate; one bound to
  // another scene is, because assigning moves it (the server is the authority
  // on whether that move is allowed).
  const all = await ApiBinary.list('mapDeviceListRequest', { arrayKey: 'devices' }).catch(() => []);
  const candidates = all.filter((d) => String(fv(d, 'scene_id') ?? '') !== String(state.sceneId));
  if (candidates.length === 0) {
    body.innerHTML = `<div class="maps-hint">${escapeHtml(t('assign_none_available'))}</div>`;
    foot.querySelector('[data-action="confirm"]')?.setAttribute('disabled', '');
  } else {
    body.innerHTML = `
      <tf-select id="maps-assign-device" label="${escapeAttr(t('field_device'))}">
        ${candidates.map((d) => {
          const nodeId = String(fv(d, 'node_id') ?? '');
          const deviceId = String(fv(d, 'device_id') ?? '');
          const label = `${String(fv(d, 'display_name') ?? '') || deviceId} · ${nodeLabel(nodeId)}`;
          return `<option value="${escapeAttr(`${nodeId}|${deviceId}`)}">${escapeHtml(label)}</option>`;
        }).join('')}
      </tf-select>
      <div class="maps-error" id="maps-assign-error" hidden></div>
    `;
  }

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || btn.hasAttribute('disabled')) return;
    if (btn.dataset.action === 'cancel') { cleanup(); return; }
    const [nodeId, deviceId] = fieldValue(body, 'maps-assign-device').split('|');
    if (!nodeId || !deviceId) { toast(t('error_device_required'), 'warn'); return; }
    const resp = await ApiBinary.action('mapDeviceAssignRequest', {
      sceneId: state.sceneId,
      nodeId,
      deviceId,
    }).catch((err) => ({ ok: false, error: err.message }));
    if (!fv(resp, 'ok')) {
      const box = body.querySelector('#maps-assign-error');
      if (box) { box.textContent = errText(resp); box.hidden = false; }
      else toast(errText(resp), 'error');
      return;
    }
    cleanup();
    toast(t('toast_device_assigned'), 'success');
    await loadDevices();
    await loadSites();
  });
}
