// =============================================================================
// File: modules/tentaquant/format.test.js
// Description: The decisions the TentaQuant screen makes before it draws
// anything — which role the instance matrix resolves the caller to, which node
// state a laboratory is in, which of the three project sections a row belongs
// to, and whether the route opens the list or one laboratory (plan §19.8).
// =============================================================================

import { I18n } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

// The screen modules resolve `/js/...` through the loader hook `_test-setup.js`
// registers, which only exists once that module has been EVALUATED — so they
// are pulled in dynamically, after it.
const {
  chooseEntryLab, initials, isSolo, labIsReady, nodeState, nodeStateLabel,
  parseServerTs, permissionSummary, roleLabel, roleOf, sectionOf, sectionProjects, shortId,
} = await import('./format.js');

const lab = (over = {}) => ({
  instanceId: 'tentaquant-0a1b2c3d',
  displayName: 'Kwanty R&D',
  enabled: true,
  myPermissions: ['quant.read', 'quant.run'],
  peopleCount: 42,
  projectCount: 3,
  lastActivityAt: '2026-09-03 14:02:00',
  nodes: [],
  ...over,
});

const project = (over = {}) => ({
  projectId: 'p1', name: 'Grover 4-kubitowy', description: '', ownerUserId: 'u1',
  ownerName: 'Anna Kowalska', visibility: 'private', myRole: 'owner',
  shareCount: 0, fileCount: 0, notebookCount: 1, runCount: 14,
  createdAt: '2026-09-01 10:00:00', updatedAt: '2026-09-03 14:02:00', archivedAt: null,
  ...over,
});

// ---------------------------------------------------------------------------
// Role resolution — the strongest granted permission names the role
// ---------------------------------------------------------------------------

test('the role is the strongest permission the matrix grants', () => {
  assert.equal(roleOf(['quant.read']), 'observer');
  assert.equal(roleOf(['quant.read', 'quant.run', 'quant.run.gpu']), 'user');
  assert.equal(roleOf(['quant.read', 'quant.run', 'quant.instruct']), 'supervisor');
  assert.equal(roleOf(['quant.read', 'quant.instruct', 'quant.admin']), 'admin');
  // A caller the matrix admits with nothing at all still reads as an observer
  // rather than crashing the tile.
  assert.equal(roleOf([]), 'observer');
  assert.equal(roleOf(undefined), 'observer');
});

test('every role has a translated label in the active locale', () => {
  assert.equal(roleLabel(['quant.read']), 'obserwator');
  assert.equal(roleLabel(['quant.read', 'quant.run']), 'użytkownik');
  assert.equal(roleLabel(['quant.read', 'quant.instruct']), 'opiekun');
  assert.equal(roleLabel(['quant.admin']), 'administrator');
});

test('the permission summary keeps manifest order and drops the quant prefix', () => {
  assert.equal(
    permissionSummary(['quant.run.qpu', 'quant.read', 'quant.run']),
    'read · run · run.qpu',
  );
  assert.equal(permissionSummary([]), '');
});

// ---------------------------------------------------------------------------
// Node and laboratory state
// ---------------------------------------------------------------------------

test('an offline node and a node whose instance is not ready are different states', () => {
  assert.equal(nodeState({ online: true, instanceStatus: 'ready' }), 'ready');
  assert.equal(nodeState({ online: true, instanceStatus: 'init_error' }), 'not_ready');
  assert.equal(nodeState({ online: false, instanceStatus: 'ready' }), 'offline');
  assert.equal(nodeState(null), 'unknown');
  assert.equal(nodeStateLabel({ online: true, instanceStatus: 'ready' }), 'gotowy');
});

test('a laboratory is ready when at least one node carries a ready instance', () => {
  assert.equal(labIsReady(lab({ nodes: [] })), false);
  assert.equal(labIsReady(lab({ nodes: [{ online: false, instanceStatus: 'ready' }] })), false);
  assert.equal(labIsReady(lab({
    nodes: [{ online: false, instanceStatus: 'ready' }, { online: true, instanceStatus: 'ready' }],
  })), true);
});

test('a one-person laboratory reads as "tylko Ty"', () => {
  assert.equal(isSolo(lab({ peopleCount: 1 })), true);
  assert.equal(isSolo(lab({ peopleCount: 0 })), true);
  assert.equal(isSolo(lab({ peopleCount: 2 })), false);
});

// ---------------------------------------------------------------------------
// Entry rule (plan §19.8)
// ---------------------------------------------------------------------------

test('one laboratory is entered directly, several show the list', () => {
  const a = lab({ instanceId: 'tentaquant-aaaaaaaa' });
  const b = lab({ instanceId: 'tentaquant-bbbbbbbb' });
  assert.equal(chooseEntryLab([a], null), 'tentaquant-aaaaaaaa');
  assert.equal(chooseEntryLab([a, b], null), null);
  assert.equal(chooseEntryLab([], null), null);
});

test('a disabled instance is not the one laboratory to enter', () => {
  const off = lab({ instanceId: 'tentaquant-aaaaaaaa', enabled: false });
  const on = lab({ instanceId: 'tentaquant-bbbbbbbb' });
  assert.equal(chooseEntryLab([off], null), null);
  assert.equal(chooseEntryLab([off, on], null), 'tentaquant-bbbbbbbb');
});

test('an explicit instance in the route wins, and an unknown one falls back to the list', () => {
  const a = lab({ instanceId: 'tentaquant-aaaaaaaa' });
  const b = lab({ instanceId: 'tentaquant-bbbbbbbb' });
  assert.equal(chooseEntryLab([a, b], 'tentaquant-bbbbbbbb'), 'tentaquant-bbbbbbbb');
  assert.equal(chooseEntryLab([a, b], 'tentaquant-cccccccc'), null);
  // A named instance is honoured even when it is the only one and disabled —
  // the laboratory view says why it is inert, the list would just hide it.
  assert.equal(chooseEntryLab([lab({ enabled: false })], 'tentaquant-0a1b2c3d'), 'tentaquant-0a1b2c3d');
});

// ---------------------------------------------------------------------------
// Project sectioning (Q03)
// ---------------------------------------------------------------------------

test('ownership, an explicit share and lab publication are three different sections', () => {
  assert.equal(sectionOf(project({ myRole: 'owner' })), 'mine');
  assert.equal(sectionOf(project({ myRole: 'owner', visibility: 'lab' })), 'mine');
  assert.equal(sectionOf(project({ myRole: 'editor' })), 'shared');
  assert.equal(sectionOf(project({ myRole: 'viewer' })), 'shared');
  assert.equal(sectionOf(project({ myRole: 'viewer', visibility: 'lab' })), 'lab');
  // An explicit editor share on a published project is more than the lab-wide
  // read everyone gets, so it belongs with what was shared by name.
  assert.equal(sectionOf(project({ myRole: 'editor', visibility: 'lab' })), 'shared');
});

test('sectionProjects splits one list into the three sections and keeps order', () => {
  const rows = [
    project({ projectId: 'a', myRole: 'owner' }),
    project({ projectId: 'b', myRole: 'viewer', visibility: 'lab' }),
    project({ projectId: 'c', myRole: 'editor' }),
    project({ projectId: 'd', myRole: 'owner', visibility: 'lab' }),
  ];
  const out = sectionProjects(rows);
  assert.deepEqual(out.mine.map((p) => p.projectId), ['a', 'd']);
  assert.deepEqual(out.shared.map((p) => p.projectId), ['c']);
  assert.deepEqual(out.lab.map((p) => p.projectId), ['b']);
  assert.deepEqual(sectionProjects(undefined), { mine: [], shared: [], lab: [] });
});

// ---------------------------------------------------------------------------
// Plural forms go through i18n, never through concatenation (rule 8)
// ---------------------------------------------------------------------------

test('Polish picks all three plural forms for the people count', () => {
  assert.equal(I18n.t('tentaquant.labs.people', { n: 1 }), '1 osoba');
  assert.equal(I18n.t('tentaquant.labs.people', { n: 3 }), '3 osoby');
  assert.equal(I18n.t('tentaquant.labs.people', { n: 42 }), '42 osoby');
  assert.equal(I18n.t('tentaquant.labs.people', { n: 12 }), '12 osób');
  assert.equal(I18n.t('tentaquant.labs.people', { n: 0 }), '0 osób');
});

test('initials never render as a single glyph', () => {
  assert.equal(initials('Anna Kowalska'), 'AK');
  assert.equal(initials('Piotr Jan Jarocki'), 'PJ');
  assert.equal(initials('ola'), 'OL');
  assert.equal(initials(''), '?');
});

// ---------------------------------------------------------------------------
// The two readings every screen shares: an id's head and a server timestamp
// ---------------------------------------------------------------------------

test('an id is shortened to the head every table prints', () => {
  assert.equal(shortId('2f9a1c3d-0000-4000-8000-000000000001'), '2f9a1c3d');
  assert.equal(shortId('7f'.repeat(32)), '7f7f7f7f', 'a node key is not a wall of hex either');
  assert.equal(shortId(null), '');
});

test('a naive Core timestamp is read as UTC — the one reading everything shares', () => {
  // `fmtDate`, `fmtAgo` and a run's measured duration all go through this, so a
  // second copy of the rule anywhere would drift from what the screen prints.
  assert.equal(parseServerTs('2026-09-03 14:02:00').getTime(), Date.parse('2026-09-03T14:02:00Z'));
  assert.equal(parseServerTs('2026-09-03T14:02:00Z').getTime(), Date.parse('2026-09-03T14:02:00Z'));
  assert.equal(parseServerTs('nonsense'), null);
  assert.equal(parseServerTs(''), null);
});
