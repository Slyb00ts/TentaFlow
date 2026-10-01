// ============ File: project-studio-tree.test.js — Permission-filtered paths and per-account view scope ============

import { window } from '../sdk-runtime/_dom-test-harness.js';
import test, { after } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { projectAncestors, projectRoots, projectTreeNodes, readTaskScope, writeTaskScope } from './project-studio-tree.js';

globalThis.localStorage = window.localStorage;

after(async () => window.happyDOM.close());

test('a child-only card has name-only ancestors without hidden siblings or decorations', () => {
  const child = { project_id: 'child', parent_id: 'hidden', path: '/hidden/child', name: 'Accessible child' };
  const breadcrumbs = [{ project_id: 'hidden', name: 'Ancestor name', depth: 1 }, { project_id: 'unrelated', name: 'Other ancestor', depth: 1 }];
  assert.deepEqual(projectRoots([child]), [child]);
  assert.deepEqual(projectAncestors(child, [child], breadcrumbs), [breadcrumbs[0]]);
  const nodes = projectTreeNodes([child], breadcrumbs, () => ({ badge: 'Current own count', draggable: true }));
  assert.equal(nodes.length, 1);
  assert.deepEqual(Object.keys(nodes[0]).sort(), ['children', 'disabled', 'id', 'label']);
  assert.equal(nodes[0].disabled, true);
  assert.equal(nodes[0].children[0].label, child.name);
  assert.equal(nodes[0].children[0].draggable, true);
});

test('authorized roots and four-level paths retain stable identities when names repeat', () => {
  const projects = ['a', 'b', 'c', 'd'].map((id, i, ids) => ({ project_id: id, parent_id: ids[i - 1] || null, path: `/${ids.slice(0, i + 1).join('/')}`, name: 'Repeated name' }));
  assert.deepEqual(projectRoots(projects).map((row) => row.project_id), ['a']);
  const tree = projectTreeNodes(projects, []);
  assert.equal(tree[0].children[0].children[0].children[0].id, 'd');
  assert.deepEqual(projectAncestors(projects[3], projects, []).map((row) => row.project_id), ['a', 'b', 'c']);
});

test('scope defaults depend on current children and persist independently by account, project and view', () => {
  localStorage.clear();
  localStorage.setItem('ps.tasks.scope.project', 'single');
  assert.equal(readTaskScope('user-one', 'project', 'list', true), 'descendants');
  assert.equal(readTaskScope('user-one', 'project', 'board', false), 'single');
  writeTaskScope('user-one', 'project', 'list', 'single');
  writeTaskScope('user-one', 'project', 'board', 'descendants');
  assert.equal(readTaskScope('user-one', 'project', 'list', true), 'single');
  assert.equal(readTaskScope('user-one', 'project', 'board', false), 'descendants');
  assert.equal(readTaskScope('user-two', 'project', 'list', true), 'descendants');
  assert.equal(readTaskScope('user-one', 'other-project', 'board', false), 'single');
  localStorage.setItem('ps.tasks.scope.user-one.project.list', 'obsolete');
  assert.equal(readTaskScope('user-one', 'project', 'list', true), 'descendants');
});

test('Project Studio keeps complete translations and named placeholders in all five languages', () => {
  const locales = ['pl', 'en', 'de', 'es', 'fr'];
  const dictionaries = locales.map((locale) => JSON.parse(readFileSync(new URL(`../../i18n/${locale}.json`, import.meta.url))).project_studio);
  const reference = dictionaries[0];
  const placeholders = (value) => [...new Set([...value.matchAll(/\{([a-zA-Z0-9_]+)(?:\|[^}]*)?\}/g)].map((match) => match[1]))].sort();
  for (const [index, dictionary] of dictionaries.entries()) {
    assert.deepEqual(Object.keys(dictionary).sort(), Object.keys(reference).sort(), locales[index]);
    for (const [key, value] of Object.entries(dictionary)) {
      assert.equal(typeof value, 'string', `${locales[index]}: ${key}`);
      assert.ok(value.trim(), `${locales[index]}: ${key}`);
      assert.deepEqual(placeholders(value), placeholders(reference[key]), `${locales[index]}: ${key}`);
    }
  }
});
