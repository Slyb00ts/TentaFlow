// =============================================================================
// File: router.test.js
// Description: The router's leave guard. `unmount` is TOLD, not asked, so a
// screen holding work that exists nowhere else — the TentaQuant notebook keeps
// its cells in the open view until a save lands — needs one place to refuse a
// navigation. `canUnmount` is that place, and these cases pin what a refusal
// does: no teardown, no mount, and an address bar that still names the screen
// the user is actually on.
// =============================================================================

// The shared bootstrap brings the happy-dom window AND the `/js/` resolver
// hook, without which router.js cannot import its absolute-path utils.
import './modules/tentaquant/_test-setup.js';
import { window } from './sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { Router } = await import('./router.js');

/// A screen that records its lifecycle, so a refused navigation can be shown to
/// have touched nothing rather than merely to have returned false.
function register(id, over = {}) {
  const screen = {
    calls: [],
    async render() { screen.calls.push('render'); return `<div data-screen="${id}"></div>`; },
    async mount() { screen.calls.push('mount'); },
    async unmount() { screen.calls.push('unmount'); },
    ...over,
  };
  Router.register(id, screen);
  return screen;
}

const asked = [];
let release = false;
const guarded = register('guarded', {
  canUnmount(next) {
    asked.push(next);
    return Promise.resolve(release);
  },
});
const plain = register('plain');
const other = register('other');

window.document.body.innerHTML = '<div id="main"></div>';

test('a navigation onto a screen with no guard just happens', async () => {
  assert.equal(await Router.navigate('plain'), true);
  assert.deepEqual(plain.calls, ['render', 'mount']);
  assert.equal(Router.current(), 'plain');
});

test('a screen that refuses to be left keeps the view it holds', async () => {
  assert.equal(await Router.navigate('guarded'), true);
  guarded.calls.length = 0;
  other.calls.length = 0;

  release = false;
  assert.equal(await Router.navigate('other'), false, 'the navigation did not happen');
  assert.deepEqual(asked, ['other'], 'and the screen was told where the user was heading');
  assert.deepEqual(guarded.calls, [], 'nothing was torn down');
  assert.deepEqual(other.calls, [], 'and nothing was mounted over it');
  assert.equal(Router.current(), 'guarded');
  assert.equal(window.location.hash, '#/guarded', 'the address bar still names the mounted screen');
});

test('re-mounting the same screen is not a leave and is never refused', async () => {
  release = true;
  assert.equal(await Router.navigate('guarded'), true);
  guarded.calls.length = 0;
  asked.length = 0;

  // The shell repaints itself onto the same view after a language change, and
  // it has already emptied #main by then — a guard that could refuse here would
  // leave the user looking at nothing.
  release = false;
  assert.equal(await Router.navigate('guarded'), true);
  assert.deepEqual(asked, [], 'the screen was not asked about leaving itself');
  assert.deepEqual(guarded.calls, ['unmount', 'render', 'mount']);
  assert.equal(Router.current(), 'guarded');
});

test('switching between two instances of one app IS a leave', async () => {
  // Two instances of a multi-instance native app share the screen id and differ
  // only by `?instance=`, so an id-only guard would let the second lab mount
  // over the first one's unsaved notebook.
  release = true;
  assert.equal(await Router.navigate('guarded', { instance: 'lab-a' }), true);
  guarded.calls.length = 0;
  asked.length = 0;

  release = false;
  assert.equal(await Router.navigate('guarded', { instance: 'lab-b' }), false);
  assert.deepEqual(asked, ['guarded'], 'the screen was asked before its own replacement');
  assert.deepEqual(guarded.calls, [], 'and lab A was left standing');
  assert.equal(window.location.hash, '#/guarded?instance=lab-a');
});

test('a screen that releases is unmounted and the next one takes over', async () => {
  release = true;
  guarded.calls.length = 0;
  other.calls.length = 0;
  assert.equal(await Router.navigate('other', { id: 'x' }), true);
  assert.deepEqual(guarded.calls, ['unmount']);
  assert.deepEqual(other.calls, ['render', 'mount']);
  assert.equal(Router.current(), 'other');
  assert.equal(window.location.hash, '#/other?id=x');
});

test('a guard that throws does not strand the router on the old screen', async () => {
  register('angry', { canUnmount() { throw new Error('nope'); } });
  await Router.navigate('angry');
  assert.equal(Router.current(), 'angry');
  // A guard is a question about unsaved work; a broken one must not become a
  // screen the user can never leave.
  assert.equal(await Router.navigate('plain'), true);
  assert.equal(Router.current(), 'plain');
});

test('an unknown view is reported as a navigation that did not happen', async () => {
  assert.equal(await Router.navigate('no-such-screen'), false);
  assert.equal(Router.current(), 'plain');
});

test('a refused navigation leaves the sidebar highlighting the screen still mounted', async () => {
  window.document.body.innerHTML = `
    <aside class="sidebar">
      <div class="nav-item" data-view="guarded"></div>
      <div class="nav-item" data-view="other"></div>
    </aside>
    <div id="main"></div>`;
  const item = (view) => window.document.querySelector(`.nav-item[data-view="${view}"]`);

  release = true;
  assert.equal(await Router.navigate('guarded'), true);
  assert.equal(item('guarded').classList.contains('active'), true, 'the router owns the highlight');
  assert.equal(item('other').classList.contains('active'), false);

  release = false;
  assert.equal(await Router.navigate('other'), false);
  // The sidebar handler in app.js must NOT move the highlight before calling
  // navigate: only the router knows whether the navigation happened, and a
  // refusal has to leave the sidebar naming the screen the user is on.
  assert.equal(item('guarded').classList.contains('active'), true, 'still the mounted screen');
  assert.equal(item('other').classList.contains('active'), false, 'not the one that was refused');
});
