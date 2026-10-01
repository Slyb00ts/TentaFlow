// ============ File: sw.lifecycle.test.js — First installation claims the current page while updates preserve the reload boundary ============

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

const source = readFileSync(new URL('../sw.js', import.meta.url), 'utf8');

function storage() {
  const rows = new Map();
  return {
    rows,
    async open(name) {
      if (!rows.has(name)) rows.set(name, new Map());
      const values = rows.get(name);
      return { put: async (key, value) => values.set(String(key), value.clone()), match: async (key) => values.get(String(key))?.clone(), delete: async (key) => values.delete(String(key)) };
    },
    keys: async () => [...rows.keys()],
    delete: async (name) => rows.delete(name),
  };
}

function worker(caches, { active = null, hash = 'first' } = {}) {
  const listeners = new Map();
  const state = { claims: 0, skips: 0 };
  const self = {
    __ASSET_BUILD_HASH: hash, __ASSET_MANIFEST: [], location: { origin: 'https://localhost' },
    registration: { active, scope: 'https://localhost/' },
    clients: { claim: async () => { state.claims += 1; } },
    skipWaiting: () => { state.skips += 1; },
    addEventListener: (name, listener) => listeners.set(name, listener),
  };
  runInNewContext(source, { self, caches, importScripts() {}, URL, Response, fetch: async () => new Response('asset') });
  const dispatch = async (name) => {
    let done;
    listeners.get(name)({ waitUntil: (promise) => { done = promise; } });
    await done;
  };
  return { state, dispatch };
}

test('first installation claims without reload even if the worker was suspended before activation', async () => {
  const caches = storage();
  const installed = worker(caches);
  await installed.dispatch('install');
  assert.equal(installed.state.claims, 0);
  const activated = worker(caches, { active: { scriptURL: '/sw.js' } });
  await activated.dispatch('activate');
  assert.equal(activated.state.claims, 1);
  assert.equal(activated.state.skips, 0);
  assert.equal(await (await caches.open('tentaflow-first')).match('https://localhost/__sw_initial_install__'), undefined);
});

test('an update never claims existing pages or skips waiting, including when old caches were cleared', async () => {
  for (const existingCache of [false, true]) {
    const caches = storage();
    if (existingCache) await caches.open('tentaflow-old');
    await caches.open('other-application');
    const update = worker(caches, { active: { scriptURL: '/sw.js' }, hash: 'updated' });
    await update.dispatch('install');
    const activated = worker(caches, { hash: 'updated' });
    await activated.dispatch('activate');
    assert.equal(activated.state.claims, 0);
    assert.equal(update.state.skips, 0);
    assert.equal(activated.state.skips, 0);
    assert.deepEqual(await caches.keys(), ['other-application', 'tentaflow-updated']);
  }
});
