// =============================================================================
// File: modules/tentaquant/project.test.js
// Description: The project shell (Q06/Q07 header, breadcrumb and tabs) and the
// Pliki tab painting under happy-dom against a fake screen. What is asserted is
// the mockup contract — the four tabs that exist, the two-line file rows, the
// footer summary — and, just as importantly, the tabs that must NOT appear
// while their backend does not.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawProject, drawFiles } = await import('./project.js');
const { fileKindOf, fileKindLabel } = await import('./files.js');

const project = (over = {}) => ({
  projectId: 'grover-4q', name: 'Grover 4-kubitowy', description: '', ownerUserId: 'u1',
  ownerName: 'Anna Kowalska', visibility: 'private', myRole: 'owner', shareCount: 0,
  fileCount: 2, notebookCount: 1, runCount: 14, linkedProjectId: null,
  createdAt: '2026-09-02 10:00:00', updatedAt: '2026-09-03 14:02:00', archivedAt: null,
  ...over,
});

const file = (over = {}) => ({
  fileId: 'f1', projectId: 'grover-4q', path: 'grover.qasm', kind: 'qasm',
  sha256: 'abc123def456789', sizeBytes: 2048, updatedAt: '2026-09-03 14:02:00',
  ...over,
});

function fakeScreen(over = {}) {
  const root = window.document.createElement('div');
  root.className = 'tq-root';
  window.document.body.appendChild(root);
  return {
    root,
    instanceId: 'tentaquant-0a1b2c3d',
    lab: { instanceId: 'tentaquant-0a1b2c3d', displayName: 'Kwanty R&D' },
    projectId: 'grover-4q',
    project: project(),
    projectTab: 'files',
    notebooks: [{ notebookId: 'nb1', name: 'Grover', currentVersion: 3, updatedAt: '2026-09-03 14:02:00' }],
    files: [file()],
    notebookId: null,
    runs: [],
    runsError: '',
    runsHost: null,
    calls: [],
    disposeProjectView() { this.calls.push('dispose'); },
    disposeRunView() { this.calls.push('dispose-run'); },
    showRuns(host, opts) { this.calls.push('runs:' + (opts?.projectId ?? '')); },
    closeProject() { this.calls.push('close'); },
    backToLabs() { this.calls.push('labs'); },
    selectProjectTab(tab) { this.calls.push('tab:' + tab); },
    openShare(id) { this.calls.push('share:' + id); },
    reloadFiles() { this.calls.push('reload-files'); },
    tq() { return Promise.resolve({}); },
    ...over,
  };
}

const cleanup = () => { window.document.body.innerHTML = ''; };

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

test('the project draws the three-level breadcrumb and only the tabs that exist', () => {
  const screen = fakeScreen();
  drawProject(screen);
  const crumbs = [...screen.root.querySelectorAll('tf-breadcrumb-item')].map((c) => c.textContent.trim());
  assert.deepEqual(crumbs, ['TentaQuant', 'Kwanty R&D', 'Projekt „Grover 4-kubitowy”']);
  const tabs = [...screen.root.querySelectorAll('#tq-project-tabs tf-tab')].map((t) => t.id);
  assert.deepEqual(tabs, ['notebook', 'studio', 'runs', 'files']);
  // The Runy tab counts what the project row says it has, and the pinned
  // gallery ("Wyniki") still has no backend, so it is still not promised.
  assert.equal(screen.root.querySelector('#tq-project-tabs tf-tab#runs').getAttribute('count'), '14');
  assert.doesNotMatch(screen.root.textContent, /Wyniki/);
  cleanup();
});

test('the Runy tab asks the screen for the listing of THIS project', () => {
  const screen = fakeScreen({ projectTab: 'runs' });
  drawProject(screen);
  assert.ok(screen.calls.includes('runs:grover-4q'), 'the project narrows the listing');
  cleanup();
});

test('the header names the project, its ownership and the counts the wire carries', () => {
  const screen = fakeScreen();
  drawProject(screen);
  const header = screen.root.querySelector('.tf-detail-header');
  assert.match(header.textContent, /Grover 4-kubitowy/);
  assert.equal(header.querySelector('tf-chip').getAttribute('label'), 'Twój projekt · prywatny');
  const chips = [...header.querySelectorAll('.d-badges tf-chip')].map((c) => c.getAttribute('label'));
  assert.deepEqual(chips, ['1 notatnik', '1 plik']);
  // Only the tier that has a backend in the browser.
  assert.deepEqual([...header.querySelectorAll('.tier')].map((t) => t.className), ['tier t0']);
  cleanup();
});

test('a shared project shows the role it was shared with instead of ownership', () => {
  const screen = fakeScreen({ project: project({ myRole: 'viewer', ownerName: 'Piotr Jarocki' }) });
  drawProject(screen);
  assert.equal(screen.root.querySelector('.tf-detail-header tf-chip').getAttribute('label'), 'Przeglądający');
  // Only the owner may share it further.
  assert.equal(screen.root.querySelector('[data-act="share"]'), null);
  cleanup();
});

test('the laboratory crumb closes the project and the root crumb leaves the laboratory', async () => {
  const screen = fakeScreen();
  drawProject(screen);
  // tf-breadcrumb paints its anchors from a MutationObserver, which happy-dom
  // runs as a microtask.
  await new Promise((resolve) => setTimeout(resolve, 0));
  const links = screen.root.querySelectorAll('tf-breadcrumb a.tf-breadcrumb-item');
  assert.equal(links.length, 2, 'the two upper levels are links');
  links[1].dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  links[0].dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(screen.calls.filter((c) => c === 'close' || c === 'labs'), ['close', 'labs']);
  cleanup();
});

// ---------------------------------------------------------------------------
// Pliki
// ---------------------------------------------------------------------------

test('the file list uses two-line cells and summarises itself in the footer', () => {
  const screen = fakeScreen({ files: [file(), file({ fileId: 'f2', path: 'notes.md', kind: 'md', sizeBytes: 512 })] });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawFiles(screen, host);
  const rows = host.querySelector('#tq-file-table').rows;
  assert.equal(rows.length, 2);
  assert.match(rows[0].name, /tf-table__cell-title/);
  assert.match(rows[0].name, /tf-table__cell-sub tf-table__cell-sub--mono/);
  assert.equal(rows[0].kind, 'OpenQASM 3');
  assert.equal(rows[1].kind, 'Markdown');
  const footer = host.querySelector('.tq-table-footer').textContent;
  assert.match(footer, /2 pliki/);
  cleanup();
});

test('an editor gets the upload control and the delete action, a viewer neither', () => {
  const editor = fakeScreen({ project: project({ myRole: 'editor' }) });
  const host = window.document.createElement('div');
  editor.root.appendChild(host);
  drawFiles(editor, host);
  assert.ok(host.querySelector('tf-file-input'), 'an editor may upload');
  // Row actions come from tf-table's own hook, not from a hand-built cell.
  assert.equal(typeof host.querySelector('#tq-file-table').rowActions, 'function');
  cleanup();

  const viewer = fakeScreen({ project: project({ myRole: 'viewer' }) });
  const vhost = window.document.createElement('div');
  viewer.root.appendChild(vhost);
  drawFiles(viewer, vhost);
  assert.equal(vhost.querySelector('tf-file-input'), null, 'a viewer never writes');
  assert.equal(vhost.querySelector('#tq-file-table').rowActions, null);
  cleanup();
});

test('an archived project is read-only even for its owner', () => {
  const screen = fakeScreen({ project: project({ archivedAt: '2026-09-03 15:00:00' }) });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawFiles(screen, host);
  assert.equal(host.querySelector('tf-file-input'), null);
  cleanup();
});

test('an empty project says so instead of drawing a headers-only table', () => {
  const screen = fakeScreen({ files: [] });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawFiles(screen, host);
  assert.equal(host.querySelector('#tq-file-table'), null);
  assert.ok(host.querySelector('tf-empty-state'));
  // Downloading is not offered while the wire has no read-back request.
  assert.doesNotMatch(host.textContent, /Pobierz/);
  cleanup();
});

// ---------------------------------------------------------------------------
// File kinds
// ---------------------------------------------------------------------------

test('a file kind comes from the wire and falls back to the extension', () => {
  assert.equal(fileKindOf('a/b/grover.qasm'), 'qasm');
  assert.equal(fileKindOf('circuit.oq3'), 'qasm');
  assert.equal(fileKindOf('run.py'), 'py');
  assert.equal(fileKindOf('notes.md'), 'md');
  assert.equal(fileKindOf('lab.ipynb'), 'notebook');
  assert.equal(fileKindOf('counts.csv'), 'data');
  assert.equal(fileKindLabel({ kind: 'py', path: 'x.qasm' }), 'py');
  assert.equal(fileKindLabel({ kind: '', path: 'x.qasm' }), 'qasm');
  assert.equal(fileKindLabel({ kind: 'nonsense', path: 'x.csv' }), 'data');
});
