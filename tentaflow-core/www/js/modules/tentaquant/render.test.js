// =============================================================================
// File: modules/tentaquant/render.test.js
// Description: The three views actually paint under happy-dom, against a fake
// screen: the laboratory grid (Q01), the laboratory dashboard (Q02) and the
// project sections (Q03). What is asserted is what the mockups promise and the
// wire can prove — the tiles, the KPI numbers, the three sections and, just as
// importantly, the ABSENCE of the cards whose backend does not exist yet.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawLabs } = await import('./labs.js');
const { drawDashboard } = await import('./dashboard.js');
const { drawProjects } = await import('./projects.js');

const lab = (over = {}) => ({
  instanceId: 'tentaquant-0a1b2c3d',
  displayName: 'Kwanty R&D',
  enabled: true,
  myPermissions: ['quant.read', 'quant.run', 'quant.run.gpu'],
  peopleCount: 42,
  projectCount: 3,
  lastActivityAt: '2026-09-03 14:02:00',
  nodes: [{ nodeId: 'n1', nodeName: 'spark-01', isLocal: true, online: true, instanceStatus: 'ready' }],
  ...over,
});

const project = (over = {}) => ({
  projectId: 'p1', name: 'Grover 4-kubitowy', description: 'Wyszukiwanie w bazie 16 elementów',
  ownerUserId: 'u1', ownerName: 'Anna Kowalska', visibility: 'private', myRole: 'owner',
  shareCount: 0, fileCount: 0, notebookCount: 1, runCount: 14,
  createdAt: '2026-09-01 10:00:00', updatedAt: '2026-09-03 14:02:00', archivedAt: null,
  ...over,
});

const overview = (over = {}) => ({
  instanceId: 'tentaquant-0a1b2c3d',
  myProjects: 3, sharedWithMe: 2, labProjects: 2,
  runs7dTotal: 37, runs7dSucceeded: 35, runs7dFailed: 1, runs7dRunning: 1,
  peopleWithAccess: 42, lastActivityAt: '2026-09-03 14:02:00',
  ...over,
});

// A stand-in for the screen shell: it records the navigation the views ask for
// instead of talking to the router or the transport.
function fakeScreen(over = {}) {
  const root = window.document.createElement('div');
  root.className = 'tq-root';
  window.document.body.appendChild(root);
  return {
    root,
    userId: 'u1',
    labs: [lab()],
    canCreate: true,
    lab: lab(),
    overview: overview(),
    overviewError: '',
    projects: [project()],
    projectsError: '',
    includeArchived: false,
    labQuery: '', labFilter: 'all', labSort: 'activity', labView: 'cards',
    projectQuery: '', projectFilter: 'all', projectSort: 'updated', projectView: 'cards',
    opened: [],
    tabs: [],
    openLab(id) { this.opened.push(id); },
    openAddons() { this.opened.push('addons'); },
    openNewLab() { this.opened.push('new-lab'); },
    openNewProject() { this.opened.push('new-project'); },
    openShare(id) { this.opened.push('share:' + id); },
    openProject(id) { this.opened.push('project:' + id); },
    selectTab(tab, opts) { this.tabs.push({ tab, opts }); },
    setArchived() {},
    setIncludeArchived() {},
    confirmDelete() {},
    ...over,
  };
}

const cleanup = () => { window.document.body.innerHTML = ''; };

// ---------------------------------------------------------------------------
// Q01 — laboratories
// ---------------------------------------------------------------------------

test('the laboratory grid draws one tile per instance plus the "new laboratory" tile', () => {
  const screen = fakeScreen({ labs: [lab(), lab({ instanceId: 'tentaquant-bbbbbbbb', displayName: 'Sandbox lokalny', peopleCount: 1 })] });
  drawLabs(screen);
  assert.equal(screen.root.querySelectorAll('.q-card[data-lab]').length, 2);
  assert.equal(screen.root.querySelectorAll('.card-new').length, 1);
  // A one-person laboratory says "tylko Ty" instead of counting to one.
  const solo = screen.root.querySelector('[data-lab="tentaquant-bbbbbbbb"]');
  assert.match(solo.textContent, /tylko Ty/);
  cleanup();
});

test('only the tiers with a backend are claimed on a tile', () => {
  const screen = fakeScreen();
  drawLabs(screen);
  const tiers = [...screen.root.querySelectorAll('.q-card .tier')].map((t) => t.textContent.trim());
  assert.deepEqual(tiers, ['T0 · przeglądarka', 'T1 · Core']);
  cleanup();
});

test('a laboratory with no ready node keeps T1 but marks it unavailable', () => {
  const screen = fakeScreen({ labs: [lab({ nodes: [{ nodeId: 'n1', nodeName: 'spark-01', isLocal: true, online: false, instanceStatus: 'ready' }] })] });
  drawLabs(screen);
  assert.ok(screen.root.querySelector('.tier.t1.off'), 'T1 is drawn as unavailable');
  assert.match(screen.root.querySelector('.q-card .qc-type').textContent, /spark-01 · offline/);
  cleanup();
});

test('clicking a tile enters that laboratory, and a disabled one does not', () => {
  const screen = fakeScreen({ labs: [lab({ enabled: false })] });
  drawLabs(screen);
  screen.root.querySelector('.q-card[data-lab]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(screen.opened, []);

  const live = fakeScreen();
  drawLabs(live);
  live.root.querySelector('.q-card[data-lab]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(live.opened, ['tentaquant-0a1b2c3d']);
  cleanup();
});

test('without the right to install addons the "new laboratory" tile is locked', () => {
  const screen = fakeScreen({ canCreate: false });
  drawLabs(screen);
  const tile = screen.root.querySelector('.card-new');
  assert.ok(tile.classList.contains('locked'));
  tile.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(screen.opened, []);
  cleanup();
});

// A laboratory is an INSTANCE of the package, so both entry points lead to the
// Addons install wizard, not to the plain Addons list.
test('the "new laboratory" tile and its toolbar button both open the install route', () => {
  const screen = fakeScreen();
  drawLabs(screen);
  screen.root.querySelector('.card-new').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  screen.root.querySelector('[data-act="new-lab"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(screen.opened, ['new-lab', 'new-lab']);
  cleanup();
});

// ---------------------------------------------------------------------------
// Q02 — laboratory dashboard
// ---------------------------------------------------------------------------

test('the dashboard shows the four counters LabOverview returns', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawDashboard(screen, host);
  const values = [...host.querySelectorAll('tf-stat-card')].map((c) => c.getAttribute('value'));
  assert.deepEqual(values, ['3', '37', '42', '2']);
  cleanup();
});

test('the dashboard draws no card for a feature without a backend', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawDashboard(screen, host);
  const text = host.textContent;
  for (const absent of ['Do zatwierdzenia', 'Urządzenia', 'Kurs', 'Konta IBM']) {
    assert.ok(!text.includes(absent), `"${absent}" is not drawn without its backend`);
  }
  // "Zacznij od" offers exactly the one action that exists.
  assert.equal(host.querySelectorAll('.start-card').length, 1);
  cleanup();
});

test('a recent project row opens that project', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawDashboard(screen, host);
  host.querySelector('.recent-row').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(screen.opened, ['project:p1']);
  cleanup();
});

// ---------------------------------------------------------------------------
// Q03 — projects
// ---------------------------------------------------------------------------

test('the projects tab renders the three sections with their cards', () => {
  const screen = fakeScreen({
    projects: [
      project({ projectId: 'a' }),
      project({ projectId: 'b', myRole: 'editor', ownerName: 'Marek Nowak' }),
      project({ projectId: 'c', myRole: 'viewer', visibility: 'lab', ownerName: 'Piotr Jarocki' }),
    ],
  });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawProjects(screen, host);
  const heads = [...host.querySelectorAll('.tq-section-head h3')].map((h) => h.textContent.trim());
  assert.deepEqual(heads, ['Moje projekty', 'Udostępnione mi', 'Materiały laboratorium']);
  assert.equal(host.querySelectorAll('.q-card[data-project]').length, 3);
  // Only the owner gets the share affordance and the row menu.
  assert.equal(host.querySelectorAll('[data-share]').length, 1);
  assert.equal(host.querySelectorAll('[data-project-menu]').length, 1);
  cleanup();
});

test('an empty section shows its own empty state, and "Moje" keeps the add tile', () => {
  const screen = fakeScreen({ projects: [] });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawProjects(screen, host);
  assert.equal(host.querySelectorAll('tf-empty-state').length, 2);
  assert.equal(host.querySelectorAll('[data-new-project]').length, 1);
  cleanup();
});

test('the share button opens the share window, and the card itself opens the project', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawProjects(screen, host);
  host.querySelector('[data-share]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(screen.opened, ['share:p1']);
  host.querySelector('.q-card[data-project] .qc-name').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(screen.opened, ['share:p1', 'project:p1']);
  cleanup();
});

test('a click on a menu item stays in the menu: the card underneath must not open', () => {
  // The menu is a child of the card, so its clicks bubble through it. Without
  // the guard 'Usun projekt' both asked for the confirm AND navigated away.
  const screen = fakeScreen({ deleted: [], confirmDelete(id) { this.deleted.push(id); } });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawProjects(screen, host);
  const item = host.querySelector('[data-project-menu] tf-menu-item[action="delete"]');
  item.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(screen.opened, []);
  item.dispatchEvent(new window.CustomEvent('action', { bubbles: true, detail: { action: 'delete' } }));
  assert.deepEqual(screen.deleted, ['p1']);
  assert.deepEqual(screen.opened, []);
  cleanup();
});

test('the footer summarises the three sections', () => {
  const screen = fakeScreen({
    projects: [
      project({ projectId: 'a' }),
      project({ projectId: 'b', myRole: 'editor' }),
      project({ projectId: 'c', myRole: 'viewer', visibility: 'lab' }),
      project({ projectId: 'd', myRole: 'viewer', visibility: 'lab' }),
    ],
  });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawProjects(screen, host);
  const footer = host.querySelector('.tq-table-footer').textContent;
  assert.match(footer, /4 projekty/);
  assert.match(footer, /1 Twój/);
  assert.match(footer, /1 udostępniony/);
  assert.match(footer, /2 materiały/);
  cleanup();
});

// ---------------------------------------------------------------------------
// List views (Q01, Q03) — cells and empty states
// ---------------------------------------------------------------------------

test('list cells use the controls.css two-line classes, the only ones a tf-table shadow root adopts', () => {
  const screen = fakeScreen({ labView: 'list' });
  drawLabs(screen);
  const [row] = screen.root.querySelector('#tq-lab-table').rows;
  assert.match(row.name, /tf-table__cell-title/);
  assert.match(row.name, /tf-table__cell-sub tf-table__cell-sub--mono/);
  assert.doesNotMatch(row.name, /tq-cell-2/, 'a feature stylesheet cannot reach into the shadow root');
  cleanup();

  const projects = fakeScreen({ projectView: 'list' });
  const panel = window.document.createElement('div');
  projects.root.appendChild(panel);
  drawProjects(projects, panel);
  const [prow] = panel.querySelector('#tq-project-table').rows;
  assert.match(prow.name, /tf-table__cell-title/);
  assert.match(prow.name, /tf-table__cell-sub tf-table__cell-sub--mono/);
  cleanup();
});

test('an empty list view draws an empty state instead of a headers-only table', () => {
  const screen = fakeScreen({ labs: [], labView: 'list' });
  drawLabs(screen);
  assert.equal(screen.root.querySelector('#tq-lab-table'), null);
  assert.ok(screen.root.querySelector('tf-empty-state'), 'Q01 says the list is empty');
  cleanup();

  const projects = fakeScreen({ projects: [], projectView: 'list' });
  const panel = window.document.createElement('div');
  projects.root.appendChild(panel);
  drawProjects(projects, panel);
  assert.equal(panel.querySelector('#tq-project-table'), null);
  const empty = panel.querySelector('tf-empty-state');
  assert.ok(empty, 'Q03 says the list is empty');
  assert.equal(empty.getAttribute('title'), 'Brak projektów na liście');
  cleanup();
});
