// =============================================================================
// File: modules/maps.test.js
// Description: The shared-map screen against a stubbed transport. Three things
// have to hold: the site list is drawn from what the wire returned, the
// mutation surfaces follow the `my_permissions` set that arrived WITH that list
// (a viewer holding only `map.read` gets no create button), and a scene delete
// the server refused because geometry exists surfaces THAT refusal and only
// re-sends with `force` after a second, explicit confirmation.
// Runs under happy-dom with the `/js/` resolver hook.
// =============================================================================

import { window } from '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { register } from 'node:module';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { dirname, resolve as pathResolve } from 'node:path';
import { readFileSync } from 'node:fs';

const here = fileURLToPath(import.meta.url);
const WWW_ROOT = pathResolve(dirname(here), '..', '..');
const hookSource = `
  const WWW_ROOT_URL = ${JSON.stringify(pathToFileURL(WWW_ROOT + '/').href)};
  export async function resolve(specifier, context, nextResolve) {
    if (specifier.startsWith('/js/')) {
      return { url: new URL('.' + specifier, WWW_ROOT_URL).href, shortCircuit: true };
    }
    return nextResolve(specifier, context);
  }
`;
register('data:text/javascript,' + encodeURIComponent(hookSource), import.meta.url);

if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver
    || class { observe() {} unobserve() {} disconnect() {} };
}
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}
if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;
if (typeof globalThis.CSS === 'undefined' && window.CSS) globalThis.CSS = window.CSS;
globalThis.fetch = (url) => {
  const m = /^\/i18n\/(\w+)\.json$/.exec(String(url));
  if (m) {
    const text = readFileSync(pathResolve(WWW_ROOT, 'i18n', `${m[1]}.json`), 'utf8');
    return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(JSON.parse(text)), text: () => Promise.resolve(text) });
  }
  return Promise.resolve({ ok: true, text: () => Promise.resolve('') });
};
if (typeof globalThis.localStorage === 'undefined') {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k),
  };
}
// codec.js starts a WASM fetch at import time that rejects under Node; the
// screen under test never reaches the codec because the transport is stubbed.
globalThis.addEventListener?.('unhandledrejection', (e) => e.preventDefault?.());
process.on('unhandledRejection', () => {});

const { I18n } = await import('../i18n.js');
await I18n.setLanguage('pl');

const { ApiBinary } = await import('../protocol/api-binary-shim.js');
const { Router } = await import('../router.js');
const { default: MapsScreen } = await import('./maps.js');

const flush = () => new Promise((r) => setTimeout(r, 0));
const t = (key, params) => I18n.t(`maps.${key}`, params);

// The screen writes its own route; the router is not mounted in this harness.
Router.replaceParams = () => {};

const calls = [];
function stubTransport(fixtures) {
  const answer = (kind, payload) => {
    calls.push({ kind, payload });
    if (!(kind in fixtures)) return Promise.reject(new Error(`unexpected request ${kind}`));
    const f = fixtures[kind];
    try {
      return Promise.resolve(typeof f === 'function' ? f(payload) : f);
    } catch (e) {
      return Promise.reject(e);
    }
  };
  ApiBinary.one = (kind, payload) => answer(kind, payload);
  ApiBinary.action = (kind, payload) => answer(kind, payload);
  ApiBinary.list = (kind, options = {}) => answer(kind, options.payload)
    .then((body) => body[options.arrayKey] ?? []);
}

const SITE = {
  site_id: 'site-1',
  name: 'Hala B',
  description: 'magazyn wysokiego składowania',
  address: 'ul. Przykładowa 4',
  lat: 52.1,
  lon: 21.0,
  alt: 110,
  scene_count: 1,
  devices_online: 2,
  last_update_ms: 1_750_000_000_000,
  created_by: 'admin',
  created_at_ms: 1_749_000_000_000,
  updated_at_ms: 1_750_000_000_000,
};

const SCENE = {
  scene_id: 'scene-1',
  site_id: 'site-1',
  name: 'Parter',
  voxel_res_m: 0.05,
  owner_node_id: 'node-local',
  owner_epoch: 3,
  geo_lat: null,
  geo_lon: null,
  geo_alt: null,
  geo_heading: null,
  max_voxels: 0,
  voxels: 1_200_000,
  chunks: 418,
  last_update_ms: 1_750_000_000_000,
  owner_online: true,
  replica_state: 'in_sync',
};

const ALL_PERMS = ['map.read', 'map.write', 'map.admin'];

const fixtures = (over = {}) => ({
  meshNodeListRequest: { nodes: [{ node_id: 'node-local', hostname: 'helios', is_local: true }] },
  mapSiteListRequest: { sites: [SITE], my_permissions: ALL_PERMS },
  mapSceneListRequest: { scenes: [SCENE] },
  mapDeviceListRequest: { devices: [] },
  ...over,
});

async function mountScreen(over = {}, params = {}) {
  calls.length = 0;
  stubTransport(fixtures(over));
  document.body.innerHTML = '<div id="main"></div>';
  const host = document.getElementById('main');
  host.innerHTML = MapsScreen.render();
  await MapsScreen.mount(params);
  for (let i = 0; i < 4; i += 1) await flush();
}

test('the site list is drawn from what the wire returned', async () => {
  await mountScreen();

  assert.ok(calls.some((c) => c.kind === 'mapSiteListRequest'), 'the screen asked for the sites');
  const table = document.getElementById('maps-sites-table');
  assert.ok(table, 'a tf-table of sites is on screen');
  assert.equal(table.rows.length, 1);
  const row = table.rows[0];
  assert.equal(row._id, 'site-1');
  assert.match(row.name, /Hala B/);
  assert.equal(row.address, 'ul. Przykładowa 4');
  assert.equal(row.scenes, 1);
  assert.equal(row.devices, 2);
  // Row actions exist because the same reply granted `map.admin`.
  assert.equal(typeof table.rowActions, 'function');

  MapsScreen.unmount();
});

test('map.admin in the reply is what puts the create button on screen', async () => {
  await mountScreen({ mapSiteListRequest: { sites: [], my_permissions: ALL_PERMS } });

  assert.ok(document.getElementById('maps-new'), 'the toolbar offers a new location');
  assert.ok(document.getElementById('maps-empty-new'), 'and so does the empty state');
  assert.equal(document.querySelector('tf-empty-state').getAttribute('message'), t('empty_admin_message'));
  // The permission set is the ONLY source — no role request is made at all.
  assert.ok(!calls.some((c) => c.kind === 'authMeRequest'), 'the screen never asks for a role');

  MapsScreen.unmount();
});

test('a viewer gets no create button and is told who can add a location', async () => {
  await mountScreen({ mapSiteListRequest: { sites: [], my_permissions: ['map.read'] } });

  assert.equal(document.getElementById('maps-new'), null, 'no create button without map.admin');
  assert.equal(document.getElementById('maps-empty-new'), null, 'and none inside the empty state');
  const empty = document.querySelector('tf-empty-state');
  assert.ok(empty, 'the empty state is shown');
  assert.equal(empty.getAttribute('message'), t('empty_viewer_message'));

  MapsScreen.unmount();
});

test('a site list that never arrives leaves every mutation hidden', async () => {
  await mountScreen({ mapSiteListRequest: () => { throw new Error('map.read permission required'); } });

  assert.equal(document.getElementById('maps-new'), null, 'nothing mutating is guessed into existence');
  assert.equal(document.getElementById('maps-empty-new'), null);

  MapsScreen.unmount();
});

test('a scene delete refused for geometry surfaces the refusal and only forces after a second confirmation', async () => {
  const refusal = 'scene holds 418 chunks of geometry; retry with force';
  const deletes = [];
  await mountScreen({
    mapSceneDeleteRequest: (payload) => {
      deletes.push(payload);
      return payload.force ? { ok: true } : { ok: false, error: refusal };
    },
  });

  // Opening the site is a row click on the sites table.
  document.getElementById('maps-sites-table').dispatchEvent(
    new window.CustomEvent('row-click', { detail: { row: { _id: 'site-1' } }, bubbles: true }),
  );
  for (let i = 0; i < 4; i += 1) await flush();

  const scenes = document.getElementById('maps-scenes-table');
  assert.ok(scenes, 'the scenes of the site are listed');
  const actions = scenes.rowActions(scenes.rows[0], 0, null);
  const del = [...actions.querySelectorAll('tf-button')].at(-1);
  assert.equal(del.textContent, t('action_delete'));
  del.click();
  await flush();

  const win = [...document.querySelectorAll('tf-window')].at(-1);
  assert.ok(win, 'the delete confirmation opened');
  const foot = win.querySelector('.maps-window-footer');
  const body = win.querySelector('.maps-window-body');
  foot.querySelector('[data-action="confirm"]').click();
  for (let i = 0; i < 4; i += 1) await flush();

  assert.deepEqual(deletes, [{ sceneId: 'scene-1', force: false }],
    'the first confirmation never carries force');
  const err = body.querySelector('#maps-scene-del-error');
  assert.equal(err.hidden, false, 'the refusal is visible');
  assert.equal(err.textContent, refusal, 'and it is the server text, not a paraphrase');
  assert.equal(body.querySelector('#maps-scene-del-force-note').hidden, false);
  assert.ok(win.isConnected, 'the window stays open so the operator can decide');

  const force = foot.querySelector('[data-action="force"]');
  assert.ok(force, 'the button now asks the harder question');
  assert.equal(force.textContent, t('action_delete_force'));
  force.click();
  for (let i = 0; i < 4; i += 1) await flush();

  assert.deepEqual(deletes, [
    { sceneId: 'scene-1', force: false },
    { sceneId: 'scene-1', force: true },
  ], 'force is sent exactly once, after the second confirmation');

  MapsScreen.unmount();
});

test('every locale carries the same maps.* keys', async () => {
  const keysOf = (lang) => {
    const text = readFileSync(pathResolve(WWW_ROOT, 'i18n', `${lang}.json`), 'utf8');
    const data = JSON.parse(text);
    assert.ok(data.nav.maps, `${lang} has nav.maps`);
    return Object.keys(data.maps).sort();
  };
  const pl = keysOf('pl');
  assert.ok(pl.length > 0, 'pl defines the namespace');
  for (const lang of ['en', 'de', 'es', 'fr']) {
    assert.deepEqual(keysOf(lang), pl, `${lang} matches pl key for key`);
  }
});
