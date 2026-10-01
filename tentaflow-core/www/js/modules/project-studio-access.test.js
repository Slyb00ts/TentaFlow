// ============ File: project-studio-access.test.js — Permission, label, and expiry projections used by Project Studio ============

import test from 'node:test';
import assert from 'node:assert/strict';
import { allowsArea, canCreateTask, projectTabs, catalogueLabel, memberGroup, expiryToInstant, expiryToLocal } from './project-studio-access.js';

test('membership without functions can create a task while its Tasks tab remains hidden', () => {
  const access = { has_access: true, archived: false, functions: [], can_create_tasks: true, areas: [{ area: 'tasks', enabled: true, level: 'none' }] };
  assert.equal(canCreateTask(access), true);
  assert.equal(allowsArea(access, 'tasks'), false);
  assert.deepEqual(projectTabs(access), ['overview', 'members']);
  assert.equal(canCreateTask({ ...access, can_create_tasks: false }), false);
  assert.equal(canCreateTask({ ...access, has_access: false }), false);
});

test('enabled server area grants control tabs and ordinary mutations independently', () => {
  const access = { hasAccess: true, archived: false, projectAdmin: true, areas: [
    { area: 'tasks', enabled: true, level: 'read' },
    { area: 'tests', enabled: false, level: 'admin' },
    { area: 'knowledge', enabled: true, level: 'write' },
    { area: 'settings', enabled: true, level: 'none' },
  ] };
  assert.deepEqual(projectTabs(access), ['overview', 'knowledge', 'tasks', 'connections', 'members']);
  assert.equal(allowsArea(access, 'tasks', 'write'), false);
  assert.equal(allowsArea(access, 'tests'), false);
  assert.equal(allowsArea(access, 'knowledge', 'write'), true);
  assert.equal(allowsArea(access, 'settings', 'write'), false, 'project_admin never synthesizes a grant in the UI');
});

test('administrator inspection and archived projects keep returned read access', () => {
  const access = { has_access: true, app_admin: true, archived: false, areas: [
    { area: 'settings', enabled: true, level: 'read' },
    { area: 'security.confidential', enabled: true, level: 'none' },
  ] };
  assert.equal(allowsArea(access, 'settings'), true);
  assert.equal(allowsArea(access, 'settings', 'write'), false);
  assert.equal(allowsArea(access, 'security.confidential'), false);
  assert.deepEqual(projectTabs(access), ['overview', 'members', 'settings']);
  assert.equal(allowsArea({ ...access, archived: true, areas: [{ area: 'settings', enabled: true, level: 'admin' }] }, 'settings', 'write'), false);
});

test('only original built-in labels are localized; customized and custom labels survive', () => {
  const translate = (key) => `translated:${key}`;
  const definition = { function_id: 'developer', name: 'Developer', description: 'Implementation and code review', builtin: true };
  assert.equal(catalogueLabel(definition, 'name', translate), 'translated:function_developer_name');
  assert.equal(catalogueLabel(definition, 'description', translate), 'translated:function_developer_description');
  assert.equal(catalogueLabel({ ...definition, name: 'Engineering' }, 'name', translate), 'Engineering');
  assert.equal(catalogueLabel({ ...definition, description: 'Hardware integration' }, 'description', translate), 'Hardware integration');
  assert.equal(catalogueLabel({ ...definition, builtin: false }, 'name', translate), 'Developer');
});

test('expired memberships form a separate group using server activity', () => {
  assert.equal(memberGroup({ active: true, expiresAt: null }), 'people');
  assert.equal(memberGroup({ active: true, expires_at: '2030-01-01T00:00:00Z' }), 'temporary');
  assert.equal(memberGroup({ active: false, expires_at: '2030-01-01T00:00:00Z' }), 'expired');
  assert.equal(memberGroup({ active: true, inherited: true }), 'inherited');
  assert.equal(memberGroup({ active: false, access: { has_access: true } }), 'expired', 'local expiry stays visible when an ancestor still grants access');
});

test('ended projects keep current reads but refuse ordinary writes and task creation', () => {
  const access = { has_access: true, ended: true, can_create_tasks: true, areas: [{ area: 'tasks', enabled: true, level: 'admin' }] };
  assert.equal(allowsArea(access, 'tasks'), true);
  assert.equal(allowsArea(access, 'tasks', 'write'), false);
  assert.equal(canCreateTask(access), false);
});

test('expiry editor preserves the local instant and rejects invalid or elapsed values', () => {
  const instant = '2030-06-01T10:20:30.000Z';
  const local = expiryToLocal(instant);
  assert.equal(expiryToInstant(local, Date.parse('2029-01-01T00:00:00Z')), instant);
  assert.equal(expiryToInstant('', 0), null);
  assert.throws(() => expiryToInstant('invalid', 0), RangeError);
  assert.throws(() => expiryToInstant(local, Date.parse(instant)), RangeError);
});

test('Board and Tasks read permissions independently expose their shared tab', () => {
  const boardOnly = { has_access: true, areas: [{ area: 'tasks', enabled: true, level: 'none' }, { area: 'board', enabled: true, level: 'write' }] };
  assert.deepEqual(projectTabs(boardOnly), ['overview', 'tasks', 'members']);
  assert.equal(allowsArea(boardOnly, 'tasks'), false);
  assert.equal(allowsArea(boardOnly, 'board', 'write'), true);
  const tasksOnly = { has_access: true, areas: [{ area: 'tasks', enabled: true, level: 'write' }, { area: 'board', enabled: true, level: 'none' }] };
  assert.deepEqual(projectTabs(tasksOnly), ['overview', 'tasks', 'members']);
  assert.equal(allowsArea(tasksOnly, 'board'), false);
});
