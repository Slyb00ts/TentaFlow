// =============================================================================
// File: modules/tentaquant/views.test.js
// Description: The filtering and ordering the two list views apply before they
// render — the laboratory grid (Q01) and the project sections (Q03). Both are
// pure functions of the wire rows, so a search or a sort can be checked without
// a DOM.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

// Dynamic, for the same reason as in format.test.js: the `/js/` loader hook is
// registered while `_test-setup.js` evaluates, and static imports resolve first.
const { matchesLab, sortLabs } = await import('./labs.js');
const { matchesProject, sortProjects } = await import('./projects.js');

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
  projectId: 'p1', name: 'Grover 4-kubitowy', description: 'Wyszukiwanie w bazie 16 elementów',
  ownerUserId: 'u1', ownerName: 'Anna Kowalska', visibility: 'private', myRole: 'owner',
  shareCount: 0, fileCount: 0, notebookCount: 1, runCount: 14,
  createdAt: '2026-09-01 10:00:00', updatedAt: '2026-09-03 14:02:00', archivedAt: null,
  ...over,
});

// ---------------------------------------------------------------------------
// Laboratory grid (Q01)
// ---------------------------------------------------------------------------

test('the laboratory search matches the display name and the instance id', () => {
  const l = lab();
  assert.equal(matchesLab(l, '', 'all'), true);
  assert.equal(matchesLab(l, 'kwanty', 'all'), true);
  assert.equal(matchesLab(l, '0a1b2c3d', 'all'), true);
  assert.equal(matchesLab(l, 'euvic', 'all'), false);
  // Whitespace alone is not a query.
  assert.equal(matchesLab(l, '   ', 'all'), true);
});

test('the supervisor filter keeps only laboratories the caller supervises', () => {
  const user = lab({ myPermissions: ['quant.read', 'quant.run'] });
  const supervisor = lab({ myPermissions: ['quant.read', 'quant.instruct'] });
  const admin = lab({ myPermissions: ['quant.admin'] });
  assert.equal(matchesLab(user, '', 'supervisor'), false);
  assert.equal(matchesLab(supervisor, '', 'supervisor'), true);
  assert.equal(matchesLab(admin, '', 'supervisor'), true);
});

test('laboratories sort by activity, name or project count', () => {
  const rows = [
    lab({ instanceId: 'b', displayName: 'Zeta', lastActivityAt: '2026-09-01 10:00:00', projectCount: 9 }),
    lab({ instanceId: 'a', displayName: 'Alfa', lastActivityAt: '2026-09-03 14:02:00', projectCount: 3 }),
    lab({ instanceId: 'c', displayName: 'Mu', lastActivityAt: null, projectCount: 5 }),
  ];
  assert.deepEqual(sortLabs(rows, 'activity').map((l) => l.instanceId), ['a', 'b', 'c']);
  assert.deepEqual(sortLabs(rows, 'name').map((l) => l.instanceId), ['a', 'c', 'b']);
  assert.deepEqual(sortLabs(rows, 'projects').map((l) => l.instanceId), ['b', 'c', 'a']);
  // Sorting never mutates the screen's own list.
  assert.deepEqual(rows.map((l) => l.instanceId), ['b', 'a', 'c']);
});

// ---------------------------------------------------------------------------
// Project sections (Q03)
// ---------------------------------------------------------------------------

test('the project search covers name, description and owner', () => {
  const p = project();
  assert.equal(matchesProject(p, 'grover'), true);
  assert.equal(matchesProject(p, '16 elementów'), true);
  assert.equal(matchesProject(p, 'kowalska'), true);
  assert.equal(matchesProject(p, 'teleportacja'), false);
  assert.equal(matchesProject(p, ''), true);
});

test('projects sort by last change, name or run count', () => {
  const rows = [
    project({ projectId: 'b', name: 'QFT vs FFT', updatedAt: '2026-09-01 09:00:00', runCount: 31 }),
    project({ projectId: 'a', name: 'Grover 4-kubitowy', updatedAt: '2026-09-03 14:02:00', runCount: 14 }),
    project({ projectId: 'c', name: 'VQE H2', updatedAt: '2026-09-02 19:40:00', runCount: 62 }),
  ];
  assert.deepEqual(sortProjects(rows, 'updated').map((p) => p.projectId), ['a', 'c', 'b']);
  assert.deepEqual(sortProjects(rows, 'name').map((p) => p.projectId), ['a', 'b', 'c']);
  assert.deepEqual(sortProjects(rows, 'runs').map((p) => p.projectId), ['c', 'b', 'a']);
  assert.deepEqual(rows.map((p) => p.projectId), ['b', 'a', 'c']);
});
