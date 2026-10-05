// =============================================================================
// File: modules/tentaquant/settings.test.js
// Description: The Ustawienia tab (Q12). Who may open it and what they may
// change follows the permissions the wire resolved; a save sends the WHOLE
// document the server last answered with only the card's own fields replaced,
// because the server rejects a partial one; and nothing is drawn for a setting
// Core does not apply (Python/GPU ceilings, timeouts, isolation, retention).
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  LIMIT_FIELDS, buildSettings, drawSettings, limitsChanged, parseLimit, readLimits, roleCounts,
  settingsAccess, sortPeople,
} = await import('./settings.js');

const SETTINGS = {
  rankingEnabled: true,
  maxQubitsBrowser: 24,
  maxQubitsCore: 28,
  maxQubitsPython: 28,
  maxQubitsGpu: 30,
  defaultTier: 'core',
  kernelIdleTtlSecs: 1800,
  cellTimeoutSecs: 300,
  gpuCellTimeoutSecs: 900,
  maxConcurrentCoreRuns: 2,
};

const PEOPLE = [
  { userId: 'u3', displayName: 'Marek Nowak', permissions: ['quant.read'] },
  { userId: 'u1', displayName: 'Anna Kowalska', permissions: ['quant.read', 'quant.run'] },
  { userId: 'u2', displayName: 'Piotr Jarocki', permissions: ['quant.read', 'quant.run', 'quant.instruct', 'quant.admin'] },
  { userId: 'u4', displayName: 'Kasia Wiśniewska', permissions: ['quant.read', 'quant.run', 'quant.instruct'] },
];

const ADMIN = ['quant.read', 'quant.run', 'quant.instruct', 'quant.admin'];
const SUPERVISOR = ['quant.read', 'quant.run', 'quant.instruct'];
const USER = ['quant.read', 'quant.run'];

// ---- pure ------------------------------------------------------------------

test('the tab is for a supervisor or an admin; limits are the admin\'s, people the supervisor\'s', () => {
  assert.deepEqual(settingsAccess(USER), { canOpen: false, canRanking: false, canLimits: false, canPeople: false });
  assert.deepEqual(settingsAccess(SUPERVISOR), { canOpen: true, canRanking: true, canLimits: false, canPeople: true });
  assert.deepEqual(settingsAccess(ADMIN), { canOpen: true, canRanking: true, canLimits: true, canPeople: true });
  assert.deepEqual(settingsAccess(['quant.read', 'quant.admin']), { canOpen: true, canRanking: true, canLimits: true, canPeople: false });
  assert.equal(settingsAccess(undefined).canOpen, false);
});

test('a save replaces only the card\'s fields and leaves the rest of the document alone', () => {
  const next = buildSettings(SETTINGS, { rankingEnabled: false });
  assert.deepEqual({ ...next, rankingEnabled: true }, SETTINGS);
  assert.equal(next.rankingEnabled, false);
  assert.equal(SETTINGS.rankingEnabled, true, 'the stored copy is not mutated');
  assert.equal(buildSettings(SETTINGS, { maxQubitsCore: 20 }).maxQubitsGpu, 30);
});

test('a limit is a whole number inside the bounds the server validates', () => {
  const [browser, core, runs] = LIMIT_FIELDS;
  assert.equal(parseLimit(browser, '24'), 24);
  assert.equal(parseLimit(browser, ' 24 '), 24);
  for (const bad of ['', '0', '41', '2.5', '-1', 'abc', '1e1']) assert.equal(parseLimit(core, bad), null, bad);
  assert.equal(parseLimit(runs, '32'), 32);
  assert.equal(parseLimit(runs, '33'), null);
});

test('limits are read together, and only a real difference counts as a change', () => {
  const same = { maxQubitsBrowser: '24', maxQubitsCore: '28', maxConcurrentCoreRuns: '2' };
  assert.deepEqual(readLimits(same), { maxQubitsBrowser: 24, maxQubitsCore: 28, maxConcurrentCoreRuns: 2 });
  assert.equal(readLimits({ ...same, maxQubitsCore: 'x' }), null);
  assert.equal(limitsChanged(SETTINGS, readLimits(same)), false);
  assert.equal(limitsChanged(SETTINGS, readLimits({ ...same, maxConcurrentCoreRuns: '4' })), true);
  assert.equal(limitsChanged(SETTINGS, null), false, 'a half-typed value is not a change');
});

test('people are sorted most privileged first and counted by role', () => {
  assert.deepEqual(sortPeople(PEOPLE).map((p) => p.userId), ['u2', 'u4', 'u1', 'u3']);
  assert.deepEqual(roleCounts(PEOPLE), { admin: 1, supervisor: 1, user: 1, observer: 1 });
  assert.deepEqual(roleCounts(null), { admin: 0, supervisor: 0, user: 0, observer: 0 });
});

// ---- the view --------------------------------------------------------------

function fakeScreen({ permissions = ADMIN, stored = SETTINGS, people = PEOPLE, set } = {}) {
  const root = window.document.createElement('div');
  root.className = 'tq-root';
  window.document.body.appendChild(root);
  const screen = {
    root,
    tab: 'settings',
    instanceId: 'tentaquant-0a1b2c3d',
    disposed: false,
    lab: { myPermissions: permissions },
    stored: structuredClone(stored),
    requests: [],
    addons: 0,
    openAddons() { this.addons += 1; },
    async tq(kind, payload = {}) {
      this.requests.push([kind, payload]);
      if (kind === 'tentaQuantSettingsGetRequest') return { settings: this.stored, admin: null };
      if (kind === 'tentaQuantLabPeopleRequest') return { people };
      if (kind === 'tentaQuantSettingsSetRequest') {
        if (set) return set(payload, this);
        this.stored = payload.settings;
        return { settings: this.stored, admin: null };
      }
      throw new Error(`unexpected ${kind}`);
    },
  };
  const host = window.document.createElement('div');
  root.appendChild(host);
  return { screen, host };
}

const cleanup = () => { window.document.body.innerHTML = ''; };
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
const click = (el) => el.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));

test('an admin sees the ranking switch, three editable limits and the people of the matrix', async () => {
  const { screen, host } = fakeScreen();
  await drawSettings(screen, host);
  assert.equal(host.querySelectorAll('.section-card').length, 3);
  assert.ok(host.querySelector('#tq-set-ranking-toggle').hasAttribute('checked'));
  assert.equal(host.querySelectorAll('#tq-set-limits tf-input').length, 3);
  assert.ok(host.querySelector('#tq-set-limits tf-input:not([disabled])'));
  assert.equal(host.querySelector('#tq-set-maxQubitsBrowser').getAttribute('value'), '24');
  assert.equal(host.querySelectorAll('.set-people tbody tr').length, 4);
  assert.match(host.querySelector('.set-people tbody tr').textContent, /Piotr Jarocki\s+administrator/);
  assert.match(host.querySelector('#tq-set-access .tq-table-footer').textContent, /4 osoby z dostępem/);
  cleanup();
});

test('nothing is drawn for a setting Core does not apply', async () => {
  const { screen, host } = fakeScreen();
  await drawSettings(screen, host);
  const text = host.textContent;
  for (const absent of [/IBM/, /QPU/, /Python/i, /GPU/, /izolac/i, /retencj/i, /timeout/i]) {
    assert.doesNotMatch(text, absent, String(absent));
  }
  cleanup();
});

test('a supervisor edits the ranking but reads the limits', async () => {
  const { screen, host } = fakeScreen({ permissions: SUPERVISOR });
  await drawSettings(screen, host);
  assert.ok(host.querySelectorAll('#tq-set-limits tf-input[disabled]').length === 3);
  assert.equal(host.querySelector('[data-act="save-limits"]'), null);
  assert.match(host.querySelector('#tq-set-limits .hint').textContent, /quant\.admin/);
  assert.equal(host.querySelector('#tq-set-ranking-toggle').hasAttribute('disabled'), false);
  cleanup();
});

test('an admin without quant.instruct is told the list of people is a supervisor\'s', async () => {
  const { screen, host } = fakeScreen({ permissions: ['quant.read', 'quant.admin'] });
  await drawSettings(screen, host);
  assert.equal(screen.requests.some(([kind]) => kind === 'tentaQuantLabPeopleRequest'), false);
  assert.match(host.querySelector('#tq-set-access').textContent, /quant\.instruct/);
  cleanup();
});

test('saving the ranking sends the whole document with only that field changed', async () => {
  const { screen, host } = fakeScreen({ permissions: SUPERVISOR });
  await drawSettings(screen, host);
  const save = host.querySelector('[data-act="save-ranking"]');
  assert.ok(save.hasAttribute('disabled'), 'nothing to save yet');
  const toggle = host.querySelector('#tq-set-ranking-toggle');
  toggle.removeAttribute('checked');
  toggle.dispatchEvent(new window.CustomEvent('change', { detail: { checked: false } }));
  assert.equal(save.hasAttribute('disabled'), false);
  click(save);
  await tick();
  await tick();

  const set = screen.requests.find(([kind]) => kind === 'tentaQuantSettingsSetRequest');
  assert.deepEqual(set[1].settings, { ...SETTINGS, rankingEnabled: false });
  assert.equal(set[1].admin, undefined, 'a supervisor never echoes the admin half');
  assert.equal(host.querySelector('#tq-set-ranking-toggle').hasAttribute('checked'), false, 'the redraw shows what Core stored');
  cleanup();
});

test('saving limits needs valid numbers and sends them with the untouched fields', async () => {
  const { screen, host } = fakeScreen();
  await drawSettings(screen, host);
  const save = host.querySelector('[data-act="save-limits"]');
  const core = host.querySelector('#tq-set-maxQubitsCore');
  core.value = 'x';
  core.dispatchEvent(new window.Event('input', { bubbles: true }));
  assert.ok(save.hasAttribute('disabled'), 'a half-typed value keeps the button down');
  core.value = '26';
  core.dispatchEvent(new window.Event('input', { bubbles: true }));
  assert.equal(save.hasAttribute('disabled'), false);
  click(save);
  await tick();
  await tick();
  const set = screen.requests.find(([kind]) => kind === 'tentaQuantSettingsSetRequest');
  assert.deepEqual(set[1].settings, { ...SETTINGS, maxQubitsCore: 26 });
  assert.equal(set[1].settings.maxQubitsGpu, 30, 'the ceilings of tiers this build lacks travel back untouched');
  cleanup();
});

test('a refusal from Core is shown under the limits and the button comes back', async () => {
  const { screen, host } = fakeScreen({
    set: () => { throw new Error('the Core tier simulates at most 28 qubits'); },
  });
  await drawSettings(screen, host);
  const save = host.querySelector('[data-act="save-limits"]');
  const core = host.querySelector('#tq-set-maxQubitsCore');
  core.value = '30';
  core.dispatchEvent(new window.Event('input', { bubbles: true }));
  click(save);
  await tick();
  await tick();
  const error = host.querySelector('#tq-set-limits-error');
  assert.equal(error.hidden, false);
  assert.match(error.textContent, /at most 28 qubits/);
  assert.equal(save.hasAttribute('disabled'), false);
  cleanup();
});

test('the access card sends the user to Addons instead of editing the matrix', async () => {
  const { screen, host } = fakeScreen();
  await drawSettings(screen, host);
  click(host.querySelector('[data-act="addons"]'));
  assert.equal(screen.addons, 1);
  cleanup();
});

test('a failed load is an alert, and an answer that arrives after the tab changed paints nothing', async () => {
  const failing = fakeScreen();
  failing.screen.tq = async () => { throw new Error('boom'); };
  await drawSettings(failing.screen, failing.host);
  assert.equal(failing.host.querySelector('tf-alert').getAttribute('message'), 'boom');

  const late = fakeScreen();
  const pending = drawSettings(late.screen, late.host);
  late.screen.tab = 'course';
  await pending;
  assert.equal(late.host.querySelector('.section-card'), null);
  cleanup();
});
