// =============================================================================
// File: modules/apps-home.test.js — Home screen mount while the screen is replaced
// =============================================================================
// The greeting waits for the signed-in user. A language change replaces the
// screen while that request is out, so the answer finds no heading to fill.

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { ApiBinary } = await import('../protocol/api-binary-shim.js');
const { default: AppsHome } = await import('./apps-home.js');

test('an answer that arrives after the screen was replaced fills nothing and throws nothing', async () => {
  let answer;
  ApiBinary.one = () => new Promise((resolve) => { answer = resolve; });
  ApiBinary.list = async () => [];
  document.body.innerHTML = `<main>${AppsHome.render()}</main>`;
  const mounted = AppsHome.mount();
  document.body.innerHTML = `<main>${AppsHome.render()}</main>`;
  document.body.innerHTML = '';
  answer({ username: 'anna', role: 'admin' });
  await assert.doesNotReject(mounted);
});

test('a screen that is still there gets its greeting and its tiles', async () => {
  ApiBinary.one = async () => ({ username: 'anna', role: 'admin' });
  ApiBinary.list = async () => [];
  document.body.innerHTML = `<main>${AppsHome.render()}</main>`;
  await AppsHome.mount();
  assert.ok(document.getElementById('apps-greeting-h').textContent.length > 0);
  assert.ok(document.querySelectorAll('#apps-grid .app-tile').length > 0);
});
