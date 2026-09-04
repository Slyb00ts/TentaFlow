// =============================================================================
// File: modules/tentaquant/dialogs.test.js
// Description: The two windows over a fake screen that records requests instead
// of sending them: "Nowy projekt" (Q04) creates a private project and offers no
// start that does not exist, and "Udostępnij projekt" (Q05) lists the shares,
// applies a role change immediately, publishes to the laboratory through the
// visibility field, says when a share is dormant, and searches every TentaFlow
// account through PeopleCandidates — warning on the ones the laboratory does
// not admit instead of hiding them.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { openNewProjectWindow, openShareWindow } = await import('./dialogs.js');

const flush = () => new Promise((r) => setTimeout(r, 0));

const project = (over = {}) => ({
  projectId: 'p1', name: 'VQE H2', description: '', ownerUserId: 'u1',
  ownerName: 'Anna Kowalska', visibility: 'private', myRole: 'owner',
  shareCount: 1, fileCount: 0, notebookCount: 3, runCount: 62,
  createdAt: '2026-08-28 10:00:00', updatedAt: '2026-09-03 14:02:00', archivedAt: null,
  ...over,
});

const share = (over = {}) => ({
  userId: 'u2', displayName: 'Kasia Wiśniewska', role: 'editor',
  grantedBy: 'Anna Kowalska', grantedAt: '2026-08-30 09:00:00', hasLabAccess: true,
  ...over,
});

// The screen shell the windows call back into: `tq` answers from `fixtures` and
// records every request, exactly as the transport-free tests of TentaNas do.
function fakeScreen(fixtures = {}, over = {}) {
  const calls = [];
  return {
    calls,
    userId: 'u1',
    lab: { instanceId: 'tentaquant-0a1b2c3d', peopleCount: 42, myPermissions: ['quant.read', 'quant.run'] },
    reloads: 0,
    tq(kind, payload = {}) {
      calls.push({ kind, payload });
      if (!(kind in fixtures)) return Promise.reject(new Error(`unexpected request ${kind}`));
      const f = fixtures[kind];
      return Promise.resolve(typeof f === 'function' ? f(payload) : f);
    },
    async reloadProjects() { this.reloads += 1; },
    ...over,
  };
}

const cleanup = () => {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  window.document.body.innerHTML = '';
};

// ---------------------------------------------------------------------------
// Q04 — new project
// ---------------------------------------------------------------------------

test('the new-project window offers only the start that exists and defaults to private', () => {
  const screen = fakeScreen();
  const win = openNewProjectWindow(screen);
  const starts = win.querySelectorAll('tf-choice-group')[0].querySelectorAll('tf-choice-card');
  assert.equal(starts.length, 1, 'no disabled fake starts');
  assert.equal(starts[0].getAttribute('value'), 'empty');
  assert.equal(win.querySelector('#tq-np-visibility').getAttribute('value'), 'private');
  cleanup();
});

test('publishing to the laboratory is offered but locked without quant.instruct', () => {
  const plain = openNewProjectWindow(fakeScreen());
  const labCard = plain.querySelector('#tq-np-visibility tf-choice-card[value="lab"]');
  assert.ok(labCard.hasAttribute('disabled'), 'a member cannot publish to the lab');
  cleanup();

  const supervisor = openNewProjectWindow(fakeScreen({}, {
    lab: { instanceId: 'tentaquant-0a1b2c3d', peopleCount: 42, myPermissions: ['quant.read', 'quant.run', 'quant.instruct'] },
  }));
  assert.ok(!supervisor.querySelector('#tq-np-visibility tf-choice-card[value="lab"]').hasAttribute('disabled'));
  cleanup();
});

test('an empty name cannot be submitted; a filled one creates the project', async () => {
  const screen = fakeScreen({ tentaQuantProjectCreateRequest: { project: project({ name: 'Grover 6-kubitowy' }) } });
  const win = openNewProjectWindow(screen);
  const submit = win.querySelector('[data-act="create"]');
  assert.ok(submit.hasAttribute('disabled'));

  const name = win.querySelector('#tq-np-name');
  name.value = 'Grover 6-kubitowy';
  name.dispatchEvent(new window.CustomEvent('input', { bubbles: true }));
  assert.ok(!submit.hasAttribute('disabled'));

  submit.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  await flush();
  assert.deepEqual(screen.calls, [{
    kind: 'tentaQuantProjectCreateRequest',
    payload: { name: 'Grover 6-kubitowy', description: '', visibility: 'private' },
  }]);
  assert.equal(screen.reloads, 1);
  cleanup();
});

// ---------------------------------------------------------------------------
// Q05 — share
// ---------------------------------------------------------------------------

const SUPERVISOR = {
  lab: {
    instanceId: 'tentaquant-0a1b2c3d',
    peopleCount: 42,
    myPermissions: ['quant.read', 'quant.run', 'quant.instruct'],
  },
};

// What PeopleCandidates answers: the whole organization, `inLab` per row.
const candidates = {
  people: [
    { userId: 'u5', displayName: 'Marek Nowak', inLab: true },
    { userId: 'u9', displayName: 'Ola Mazur', inLab: false },
  ],
};

const typeSearch = (win, value) => win.querySelector('#tq-share-search')
  .dispatchEvent(new window.CustomEvent('search', { bubbles: true, detail: { value } }));


test('the share window lists the owner and every share with its role', async () => {
  const screen = fakeScreen({
    tentaQuantProjectGetRequest: { project: project(), shares: [share(), share({ userId: 'u3', displayName: 'Marek Nowak', role: 'viewer' })] },
  });
  const win = openShareWindow(screen, 'p1');
  await flush();
  const rows = win.querySelectorAll('.tq-share-table tbody tr');
  assert.equal(rows.length, 3, 'owner plus two shares');
  assert.match(rows[0].textContent, /Anna Kowalska/);
  assert.match(rows[0].textContent, /nie można usunąć/);
  const roles = [...win.querySelectorAll('[data-role-for]')].map((s) => s.getAttribute('value'));
  assert.deepEqual(roles, ['editor', 'viewer']);
  cleanup();
});

test('a person without lab access is marked dormant and explained', async () => {
  const screen = fakeScreen({
    tentaQuantProjectGetRequest: { project: project(), shares: [share({ userId: 'u4', displayName: 'Ola Mazur', hasLabAccess: false })] },
  });
  const win = openShareWindow(screen, 'p1');
  await flush();
  // tf-chip carries its text in the `label` attribute, so the row is checked by
  // the chip it grew, not by the window's text.
  const chip = win.querySelector('[data-share-user="u4"] tf-chip[status="warn"]');
  assert.ok(chip, 'the dormant share is chipped');
  assert.equal(chip.getAttribute('label'), 'uśpione');
  const alert = [...win.querySelectorAll('tf-alert')].find((a) => a.getAttribute('tone') === 'warning');
  assert.ok(alert, 'the dormant share is explained');
  assert.match(alert.getAttribute('message'), /Ola Mazur/);
  assert.match(alert.getAttribute('message'), /quant\.read/);
  cleanup();
});

test('changing a role sends ShareSet and refreshes the list', async () => {
  const screen = fakeScreen({
    tentaQuantProjectGetRequest: { project: project(), shares: [share()] },
    tentaQuantProjectShareSetRequest: { shares: [share({ role: 'viewer' })] },
  });
  const win = openShareWindow(screen, 'p1');
  await flush();
  const select = win.querySelector('[data-role-for="u2"]');
  select.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'viewer' } }));
  await flush();
  assert.deepEqual(screen.calls[1], {
    kind: 'tentaQuantProjectShareSetRequest',
    payload: { projectId: 'p1', userId: 'u2', role: 'viewer' },
  });
  cleanup();
});

test('the laboratory toggle publishes through the project visibility field', async () => {
  const screen = fakeScreen({
    tentaQuantProjectGetRequest: { project: project(), shares: [] },
    tentaQuantProjectUpdateRequest: { project: project({ visibility: 'lab' }) },
  }, {
    lab: { instanceId: 'tentaquant-0a1b2c3d', peopleCount: 42, myPermissions: ['quant.read', 'quant.run', 'quant.instruct'] },
  });
  const win = openShareWindow(screen, 'p1');
  await flush();
  const toggle = win.querySelector('#tq-share-lab');
  assert.ok(!toggle.hasAttribute('disabled'));
  toggle.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked: true } }));
  await flush();
  assert.deepEqual(screen.calls[1], {
    kind: 'tentaQuantProjectUpdateRequest',
    payload: { projectId: 'p1', name: 'VQE H2', description: '', visibility: 'lab' },
  });
  cleanup();
});

test('without quant.instruct the laboratory toggle is disabled and says why', async () => {
  const screen = fakeScreen({ tentaQuantProjectGetRequest: { project: project(), shares: [] } });
  const win = openShareWindow(screen, 'p1');
  await flush();
  assert.ok(win.querySelector('#tq-share-lab').hasAttribute('disabled'));
  assert.match(win.querySelector('.toggle-row .tr-sub').textContent, /quant\.instruct/);
  cleanup();
});

test('the role legend names all three project roles', async () => {
  const screen = fakeScreen({ tentaQuantProjectGetRequest: { project: project(), shares: [] } });
  const win = openShareWindow(screen, 'p1');
  await flush();
  const titles = [...win.querySelectorAll('.role-legend .rt')].map((r) => r.textContent.trim());
  assert.deepEqual(titles, ['Właściciel', 'Edytor', 'Przeglądający']);
  // Sharing never grants laboratory access — the window has to say so.
  assert.ok([...win.querySelectorAll('tf-alert')].some((a) => /Addons/.test(a.getAttribute('message') || '')));
  cleanup();
});

test('every owner gets the picker — it does not need quant.instruct', async () => {
  const screen = fakeScreen({
    tentaQuantProjectGetRequest: { project: project(), shares: [] },
    tentaQuantPeopleCandidatesRequest: candidates,
  });
  const win = openShareWindow(screen, 'p1');
  await flush();
  assert.ok(win.querySelector('#tq-share-search'), 'an ordinary owner searches too');
  // Nothing is asked before the user types: the picker opens empty.
  assert.ok(!screen.calls.some((c) => c.kind === 'tentaQuantPeopleCandidatesRequest'));
  assert.ok(
    !win.querySelectorAll('tf-alert').length
      || ![...win.querySelectorAll('tf-alert')].some((a) => /quant\.instruct/.test(a.getAttribute('message') || '')),
    'and is never told to ask a supervisor',
  );
  cleanup();
});

test('typing searches every TentaFlow account and shares with one click', async () => {
  const screen = fakeScreen({
    tentaQuantProjectGetRequest: { project: project(), shares: [share()] },
    tentaQuantPeopleCandidatesRequest: candidates,
    tentaQuantProjectShareSetRequest: { shares: [share(), share({ userId: 'u5', displayName: 'Marek Nowak', role: 'viewer' })] },
  });
  const win = openShareWindow(screen, 'p1');
  await flush();

  typeSearch(win, 'no');
  await flush();
  assert.deepEqual(
    screen.calls.filter((c) => c.kind === 'tentaQuantPeopleCandidatesRequest'),
    [{ kind: 'tentaQuantPeopleCandidatesRequest', payload: { query: 'no', limit: 12 } }],
    'the query and the row cap travel to the server',
  );
  const rows = [...win.querySelectorAll('[data-candidate]')];
  assert.deepEqual(rows.map((r) => r.dataset.candidate), ['u5', 'u9']);

  rows[0].dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  await flush();
  assert.deepEqual(screen.calls.at(-1), {
    kind: 'tentaQuantProjectShareSetRequest',
    payload: { projectId: 'p1', userId: 'u5', role: 'viewer' },
  });
  assert.equal(win.querySelectorAll('.tq-share-table tbody tr').length, 3, 'the table follows the answer');
  cleanup();
});

test('a candidate without laboratory access stays selectable and carries the warning', async () => {
  const screen = fakeScreen({
    tentaQuantProjectGetRequest: { project: project(), shares: [] },
    tentaQuantPeopleCandidatesRequest: candidates,
    tentaQuantProjectShareSetRequest: { shares: [share({ userId: 'u9', displayName: 'Ola Mazur', role: 'viewer', hasLabAccess: false })] },
  });
  const win = openShareWindow(screen, 'p1');
  await flush();
  typeSearch(win, 'o');
  await flush();

  const outside = win.querySelector('[data-candidate="u9"]');
  assert.ok(outside.classList.contains('is-outside'));
  const warning = outside.querySelector('.tq-candidate-warning').textContent;
  assert.match(warning, /Ola Mazur/);
  assert.match(warning, /nie ma dostępu do laboratorium/);
  assert.match(warning, /Addons/);
  // The row that warns is still the row that shares.
  assert.ok(!win.querySelector('[data-candidate="u5"] .tq-candidate-warning'), 'a member has nothing to warn about');
  outside.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  await flush();
  assert.deepEqual(screen.calls.at(-1), {
    kind: 'tentaQuantProjectShareSetRequest',
    payload: { projectId: 'p1', userId: 'u9', role: 'viewer' },
  });
  cleanup();
});

test('only one search is in flight and the last query wins', async () => {
  let release;
  const first = new Promise((r) => { release = r; });
  const asked = [];
  const screen = fakeScreen({
    tentaQuantProjectGetRequest: { project: project(), shares: [] },
    tentaQuantPeopleCandidatesRequest: (payload) => {
      asked.push(payload.query);
      return asked.length === 1
        ? first.then(() => ({ people: [{ userId: 'u1x', displayName: 'Stale', inLab: true }] }))
        : Promise.resolve(candidates);
    },
  });
  const win = openShareWindow(screen, 'p1');
  await flush();

  typeSearch(win, 'ma');
  typeSearch(win, 'mar');
  typeSearch(win, 'marek');
  await flush();
  assert.deepEqual(asked, ['ma'], 'the keystrokes queue instead of racing');

  release();
  await flush();
  await flush();
  assert.deepEqual(asked, ['ma', 'marek'], 'and only the last query is asked again');
  // The stale answer never reached the list.
  assert.deepEqual(
    [...win.querySelectorAll('[data-candidate]')].map((r) => r.dataset.candidate),
    ['u5', 'u9'],
  );
  cleanup();
});

test('the search result survives a share change and drops people already added', async () => {
  const screen = fakeScreen({
    tentaQuantProjectGetRequest: { project: project(), shares: [share()] },
    tentaQuantPeopleCandidatesRequest: candidates,
    tentaQuantProjectShareRemoveRequest: { shares: [] },
  });
  const win = openShareWindow(screen, 'p1');
  await flush();

  typeSearch(win, 'marek');
  await flush();
  const box = win.querySelector('#tq-share-search');
  win.querySelector('[data-remove-share="u2"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  await flush();

  // The search box is the SAME element after the mutation: what the user typed
  // lives inside it, and a full redraw of the window would have thrown it away.
  assert.equal(win.querySelector('#tq-share-search'), box, 'the search box survived the change');
  assert.deepEqual([...win.querySelectorAll('[data-candidate]')].map((r) => r.dataset.candidate), ['u5', 'u9']);
  assert.equal(
    screen.calls.filter((c) => c.kind === 'tentaQuantPeopleCandidatesRequest').length,
    1,
    'a share change does not re-run the search',
  );
  cleanup();
});

test('a failed publish puts the toggle back where the server left it', async () => {
  const screen = fakeScreen({
    tentaQuantProjectGetRequest: { project: project(), shares: [] },
  }, SUPERVISOR);
  const win = openShareWindow(screen, 'p1');
  await flush();
  const toggle = win.querySelector('#tq-share-lab');
  toggle.setAttribute('checked', '');
  toggle.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked: true } }));
  await flush();
  assert.ok(!toggle.hasAttribute('checked'), 'the project is still private, so the toggle is off');
  assert.match(win.querySelector('[data-error]').textContent, /tentaQuantProjectUpdateRequest/);
  cleanup();
});
