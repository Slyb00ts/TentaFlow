// ============ File: project-studio.test.js — Project access and member editor behavior through the real screen ============

import { window } from '../sdk-runtime/_dom-test-harness.js';
import test, { after, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { PROJECT_AREAS, BUILTIN_FUNCTIONS } from './project-studio-access.js';

for (const name of ['MutationObserver', 'ResizeObserver', 'Document', 'CSS', 'navigator']) {
  if (globalThis[name] === undefined) globalThis[name] = window[name];
}
Object.defineProperty(globalThis, 'localStorage', { value: window.localStorage, configurable: true });
// happy-dom has no canvas backend; the code editor only needs character width.
window.HTMLCanvasElement.prototype.getContext = () => ({ measureText: (text) => ({ width: String(text).length * 7 }) });
const hostFetch = globalThis.fetch;
globalThis.fetch = (url, options) => {
  const match = /^\/i18n\/(\w+)\.json$/.exec(String(url));
  if (match) return Promise.resolve(new Response(readFileSync(new URL(`../../i18n/${match[1]}.json`, import.meta.url)), { headers: { 'Content-Type': 'application/json' } }));
  if (String(url).startsWith('/css/')) return Promise.resolve(new Response(readFileSync(new URL(`../..${url}`, import.meta.url)), { headers: { 'Content-Type': 'text/css' } }));
  return hostFetch(url, options);
};
const { I18n } = await import('../i18n.js');
localStorage.setItem('tentaflow_lang', 'en');
await I18n.init();
const { ApiBinary } = await import('../protocol/api-binary-shim.js');
const { Router } = await import('../router.js');
const { default: screen } = await import('./project-studio.js');
afterEach(() => screen.unmount());
after(async () => { screen.unmount(); await window.happyDOM.close(); });
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const calls = [];
const navigation = [];
ApiBinary.client = async () => ({ addUnsolicitedListener() {} });
ApiBinary.subscribe = async (kind, payload) => { calls.push({ kind, payload }); throw new Error(`unexpected stream ${kind}`); };
Router.navigate = async (view, params) => { navigation.push({ view, params }); return true; };

function access(overrides = {}) {
  return { hasAccess: true, projectAdmin: false, appAdmin: false, isOwner: false,
    archived: false, ended: false, functions: [], expiresAt: null, enabledModules: ['knowledge', 'tasks'],
    areas: PROJECT_AREAS.map((area) => ({ area, enabled: ['knowledge', 'repos', 'tasks', 'board', 'sprints', 'settings'].includes(area), level: 'none' })),
    canCreateTasks: true, canManageMembers: false, canManageSettings: false, ...overrides };
}

function catalogue() {
  return BUILTIN_FUNCTIONS.map((definition) => ({ ...definition,
    functionId: definition.functionId,
    name: definition.functionId === 'developer' ? 'Engineering' : definition.name,
    grants: PROJECT_AREAS.map((area) => ({ area, level: area === 'knowledge' ? 'read' : 'none' })),
  }));
}

function taskTypes() {
  return ['feature', 'defect', 'technical', 'security', 'subtask', 'epic'].map((typeId, i) => ({ typeId, name: typeId, description: typeId, sortOrder: i * 10, builtIn: true, active: true }));
}

function projectFixture(projectAccess, overrides = {}) {
  return { parentId: null, path: '/p0-project', depth: 1, lifecycle: projectAccess.ended ? 'ended' : 'active', endedAt: null, isPrivate: false, inheritModules: false, inheritTaskTypes: false, taskTypeSourceProjectId: 'p0-project', ownOpenTasks: 0, descendantOpenTasks: 0, myOpenTasks: 0, overdueTasks: 0, canCreateChild: projectAccess.projectAdmin && !projectAccess.archived && !projectAccess.ended, canMove: projectAccess.isOwner && !projectAccess.archived && !projectAccess.ended, canArchive: projectAccess.isOwner && !projectAccess.archived && !projectAccess.ended, canUnarchive: projectAccess.isOwner && projectAccess.archived, canDelete: projectAccess.isOwner, canEnd: projectAccess.isOwner && !projectAccess.archived && !projectAccess.ended, canResume: projectAccess.isOwner && projectAccess.ended, canExport: projectAccess.projectAdmin && projectAccess.hasAccess, projectId: 'p0-project', name: 'Workflow', description: '', template: 'custom', status: projectAccess.archived ? 'archived' : 'active', modules: ['knowledge', 'tasks'], keyPrefix: 'WF', keyPrefixLocked: false, access: projectAccess, memberCount: 3, ...overrides };
}

async function mount(projectAccess, overrides = {}, params = null) {
  screen.unmount();
  calls.length = 0;
  navigation.length = 0;
  const project = projectFixture(projectAccess);
  const fixtures = {
    // An AuthMe role never substitutes for project or application permissions.
    authMeRequest: { userId: new Uint8Array(16), username: 'Creator', role: 'admin' },
    projectStudioProjectTreeRequest: { projects: [project], breadcrumbs: [], canCreate: false, canAdminister: false },
    projectStudioCatalogueGetRequest: { functions: catalogue() },
    projectStudioProjectGetRequest: { project },
    projectStudioOverviewRequest: { kpis: {}, activity: [] },
    projectStudioNotificationsListRequest: { notifications: [], unreadCount: 0 },
    projectStudioMembersListRequest: { members: [] },
    projectStudioTaskSaveRequest: { taskId: 'created-task', taskNo: 1, taskKey: 'WF-1', eventIds: [1] },
    projectStudioTaskTypesListRequest: { types: taskTypes(), sourceProjectId: 'p0-project' },
    projectStudioTaskKeyResolveRequest: (input) => {
      assert.notEqual(input.taskKey != null, input.taskId != null, 'the resolver receives exactly one task identity');
      assert.equal(input.originProjectId != null, input.originEventId != null, 'origin metadata is supplied as a complete pair');
      return { projectId: 'p0-project', taskId: input.taskId || 'task-one', currentKey: input.taskKey || 'WF-7', eventId: input.originEventId == null ? null : Number(input.originEventId) };
    },
    projectStudioProjectInheritancePreviewRequest: { currentModules: project.modules, proposedModules: project.modules, currentTaskTypes: taskTypes(), proposedTaskTypes: taskTypes() },
    ...overrides,
  };
  ApiBinary.one = async (kind, payload) => {
    calls.push({ kind, payload });
    if (!(kind in fixtures)) throw new Error(`unexpected request ${kind}`);
    const result = await (typeof fixtures[kind] === 'function' ? fixtures[kind](payload) : fixtures[kind]);
    if (kind === 'projectStudioTasksListRequest') {
      const rows = result.tasks.map((task) => ({ projectId: 'p0-project', projectName: 'Workflow', taskTypeName: task.taskType, resolution: null, resolutionReason: null, ...task }));
      const summary = { todo: rows.filter((task) => task.status === 'todo').length, inProgress: rows.filter((task) => task.status === 'in_progress').length, review: rows.filter((task) => task.status === 'review').length, done: rows.filter((task) => task.status === 'done').length, ownTotal: result.total, descendantTotal: 0, sourceRevision: 1, appliedRevision: 1, indexedAt: '2026-10-01T12:00:00Z' };
      return { summary, ...result, tasks: rows };
    }
    return result;
  };
  document.body.innerHTML = `<main>${screen.render()}</main>`;
  await screen.mount(params || {});
  if (params) return;
  document.querySelector('#ps-filter').dispatchEvent(new CustomEvent('change', { detail: { id: 'all' }, bubbles: true }));
  for (let i = 0; i < 4; i += 1) await flush();
  document.querySelector('[data-project-id]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 8; i += 1) await flush();
}

async function openTestsView(view = 'cases') {
  document.querySelector('#ps-project-tabs').dispatchEvent(new CustomEvent('change', { detail: { value: 'tests' }, bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  if (view !== 'cases') {
    document.querySelector('#ps-tests-seg').dispatchEvent(new CustomEvent('change', { detail: { value: view }, bubbles: true }));
    for (let i = 0; i < 6; i += 1) await flush();
  }
}

function testAccess(testLevel = 'write') {
  return access({ areas: PROJECT_AREAS.map((area) => ({ area, enabled: true, level: area === 'tests' ? testLevel : area === 'settings' ? 'read' : 'none' })) });
}

test('a member with no functions creates through the header without unreadable requests', async () => {
  await mount(access());
  assert.deepEqual([...document.querySelectorAll('#ps-project-tabs tf-tab')].map((tab) => tab.id), ['overview', 'members']);
  assert.equal(document.querySelector('[data-export]'), null);
  assert.equal(document.querySelector('[data-goto-settings]'), null);
  const create = document.querySelector('[data-new-task]');
  assert.ok(create, 'task creation is reachable outside the hidden Tasks tab');
  create.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 4; i += 1) await flush();
  const title = document.querySelector('#ps-task-title');
  assert.ok(title);
  title.value = 'A task from a member without functions';
  title.dispatchEvent(new Event('input', { bubbles: true }));
  assert.ok(document.querySelector('#ps-task-att-input'), 'creation includes the authorized staged upload path');
  assert.equal(document.querySelector('#ps-task-parent'), null, 'parent search does not imply Tasks Read');
  assert.ok(document.querySelector('#ps-task-type option[value="subtask"]').hasAttribute('disabled'));
  document.querySelector('tf-window [data-action="save"]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 4; i += 1) await flush();
  assert.equal(calls.filter((call) => call.kind === 'projectStudioTaskSaveRequest').length, 1);
  assert.equal(calls.some((call) => ['projectStudioTaskGetRequest', 'projectStudioTasksListRequest', 'projectStudioCasesListRequest', 'projectStudioSourcesListRequest'].includes(call.kind)), false);
  await new Promise((resolve) => setTimeout(resolve, 280));
  assert.ok(!document.querySelector('.ps-window-body #ps-task-title'), 'successful creation closes the form');
  screen.unmount();
});

test('members are grouped by actual active status and removal enters shared handover', async () => {
  const adminAccess = access({ projectAdmin: true, canManageMembers: true });
  const members = [
    { userId: 'permanent', displayName: 'Permanent', active: true, functions: ['developer'], projectAdmin: false, expiresAt: null, isOwner: false, access: access() },
    { userId: 'temporary', displayName: 'Temporary', active: true, functions: ['tester', 'developer'], projectAdmin: false, expiresAt: '2030-01-01T00:00:00Z', isOwner: false, access: access() },
    { userId: 'expired', displayName: 'Expired', active: false, functions: ['tester'], projectAdmin: false, expiresAt: '2020-01-01T00:00:00Z', isOwner: false, access: access({ hasAccess: false, canCreateTasks: false }) },
  ];
  await mount(adminAccess, { projectStudioMembersListRequest: { members } });
  document.querySelector('[data-goto-members]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  for (const [group, id] of [['people', 'permanent'], ['temporary', 'temporary'], ['expired', 'expired']]) {
    const table = document.querySelector(`[data-member-table="${group}"]`);
    assert.equal(table.rows.length, 1);
    assert.equal(table.rows[0]._id, id);
  }
  const table = document.querySelector('[data-member-table="people"]');
  assert.match(table.rows[0].functions, /Engineering/, 'an edited built-in name is retained');
  const button = table.rowActions(table.rows[0]);
  document.body.appendChild(button);
  button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  const item = [...document.querySelectorAll('tf-menu tf-menu-item')].find((row) => row.getAttribute('label') === I18n.t('project_studio.members_handover'));
  assert.ok(item);
  item.closest('tf-menu').dispatchEvent(new CustomEvent('action', { detail: { action: item.getAttribute('action') }, bubbles: true }));
  assert.deepEqual(navigation[0], { view: 'org-structure', params: { tab: 'list', handover: 'permanent', reason: 'project_removal', project: 'p0-project' } });
  assert.equal(calls.some((call) => call.kind === 'projectStudioMemberRemoveRequest'), false);
  screen.unmount();
});

test('the member editor saves multiple functions, administration and a precise expiry together', async () => {
  const member = { userId: 'temporary', displayName: 'Temporary', active: true, functions: ['tester'], projectAdmin: false, expiresAt: null, isOwner: false, access: access() };
  await mount(access({ projectAdmin: true, canManageMembers: true }), {
    projectStudioMembersListRequest: { members: [member] }, projectStudioMemberAccessSetRequest: { ok: true },
  });
  document.querySelector('[data-goto-members]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const table = document.querySelector('[data-member-table="people"]');
  const button = table.rowActions(table.rows[0]);
  document.body.appendChild(button);
  button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  const item = [...document.querySelectorAll('tf-menu tf-menu-item')].find((row) => row.getAttribute('label') === I18n.t('project_studio.access_edit'));
  item.closest('tf-menu').dispatchEvent(new CustomEvent('action', { detail: { action: item.getAttribute('action') }, bubbles: true }));
  document.querySelector('[data-member-function="developer"]').checked = true;
  document.querySelector('[data-project-admin]').checked = true;
  const localExpiry = '2030-06-01T12:00:00';
  document.querySelector('[data-member-expiry]').value = localExpiry;
  document.querySelector('tf-window [data-action="save"]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const saved = calls.find((call) => call.kind === 'projectStudioMemberAccessSetRequest');
  assert.deepEqual(saved.payload, { projectId: 'p0-project', userId: 'temporary', functions: ['developer', 'tester'], projectAdmin: true, expiresAt: new Date(localExpiry).toISOString() });
  assert.equal('role' in saved.payload, false);
  screen.unmount();
});

test('an archived owner can inspect members and export without offering ownership transfer', async () => {
  const member = { userId: 'successor', displayName: 'Successor', active: true, functions: ['tester'], projectAdmin: false, expiresAt: null, isOwner: false, access: access({ archived: true, canCreateTasks: false }) };
  await mount(access({ isOwner: true, projectAdmin: true, archived: true, canCreateTasks: false }), {
    projectStudioMembersListRequest: { members: [member] },
  });
  assert.ok(document.querySelector('[data-export]'), 'actual project administrators retain archive export');
  assert.ok(!document.querySelector('[data-new-task]'));
  document.querySelector('[data-goto-members]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const table = document.querySelector('[data-member-table="people"]');
  assert.equal(table.rows[0]._id, 'successor');
  assert.equal(table.rowActions, null, 'the owner has no active member action in an archived project');
  assert.ok(!document.querySelector('#ps-tab-panel tf-menu'));
  assert.equal(calls.some((call) => call.kind === 'projectStudioOwnershipTransferRequest'), false);
  screen.unmount();
});

test('Chat Write without Knowledge Read keeps history but cannot create or send a conversation', async () => {
  const chatAccess = access({ areas: PROJECT_AREAS.map((area) => ({ area, enabled: true, level: area === 'chat' ? 'write' : 'none' })) });
  await mount(chatAccess, {
    projectStudioChatsListRequest: { chats: [{ chatId: 'saved-chat', title: 'Saved conversation' }] },
    projectStudioChatHistoryRequest: { messages: [{ role: 'assistant', content: 'Saved answer', createdAt: '2026-09-30T12:00:00Z', citationsJson: '[]' }] },
  });
  document.querySelector('#ps-project-tabs').dispatchEvent(new CustomEvent('change', { detail: { value: 'chat' }, bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const composer = document.querySelector('#ps-chat-composer');
  assert.ok(composer.hasAttribute('disabled'));
  assert.ok(document.querySelector('#ps-chat-new').hidden);
  composer.dispatchEvent(new CustomEvent('send', { detail: { text: 'No empty conversation should be created' }, bubbles: true }));
  await flush();
  document.querySelector('[data-conv="saved-chat"]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 4; i += 1) await flush();
  assert.equal(calls.filter((call) => call.kind === 'projectStudioChatHistoryRequest').length, 1);
  assert.match(document.querySelector('#ps-chat-msgs').textContent, /Saved answer/);
  assert.equal(calls.some((call) => ['projectStudioChatCreateRequest', 'projectStudioChatStreamRequest'].includes(call.kind)), false);
  screen.unmount();
});

test('Tests Write without Environments Read edits a code case without offering a try run', async () => {
  const testAccess = access({ areas: PROJECT_AREAS.map((area) => ({ area, enabled: true, level: area === 'tests' ? 'write' : area === 'settings' ? 'read' : 'none' })) });
  const info = { caseId: 'code-case', title: 'Code case', kind: 'api', status: 'draft', priority: 'medium', currentVersion: 1, language: 'python', tagIds: [], linkedSourceIds: [] };
  await mount(testAccess, {
    projectStudioCasesListRequest: { cases: [info], total: 1 },
    projectStudioCaseGetRequest: { detail: { info, contentJson: JSON.stringify({ script: 'assert True', language: 'python', config: {} }), attachments: [], versions: [] } },
    projectStudioSettingsGetRequest: { settings: { tags: [] } },
    projectStudioRunnersListRequest: { runners: [{ toolchains: [{ language: 'python', frameworks: ['pytest'] }] }] },
  });
  document.querySelector('#ps-project-tabs').dispatchEvent(new CustomEvent('change', { detail: { value: 'tests' }, bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const table = document.querySelector('#ps-cases-table');
  assert.ok(table);
  table.dispatchEvent(new CustomEvent('row-click', { detail: { row: table.rows[0] }, bubbles: true }));
  for (let i = 0; i < 8; i += 1) await flush();
  const start = document.querySelector('#ps-code-try');
  assert.ok(start?.hasAttribute('disabled'));
  assert.ok(!document.querySelector('#ps-code-env'));
  assert.ok(!document.querySelector('tf-code-editor').hasAttribute('readonly'), 'test content remains editable');
  start.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await flush();
  assert.equal(calls.some((call) => ['projectStudioEnvironmentsListRequest', 'projectStudioTryRunStartRequest'].includes(call.kind)), false);
  screen.unmount();
});

test('Tests Write without Knowledge Read retains cases and generations without opening a generator', async () => {
  await mount(testAccess(), {
    projectStudioSettingsGetRequest: { settings: { tags: [] } },
    projectStudioCasesListRequest: { cases: [], total: 0 },
    projectStudioGenerationsListRequest: { generations: [] },
  });
  await openTestsView();
  const create = document.querySelector('#ps-cases-new');
  assert.ok(create && !create.hasAttribute('disabled'), 'ordinary case editing remains available');
  const generate = document.querySelector('#ps-cases-generate');
  assert.ok(generate.hasAttribute('disabled'));
  generate.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await flush();
  assert.ok(!document.querySelector('#ps-gen-count'));
  document.querySelector('#ps-tests-seg').dispatchEvent(new CustomEvent('change', { detail: { value: 'generations' }, bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const next = document.querySelector('#ps-gens-new');
  assert.ok(next.hasAttribute('disabled'));
  next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await flush();
  assert.ok(!document.querySelector('#ps-gen-count'));
  assert.equal(calls.some((call) => ['projectStudioSourcesListRequest', 'projectStudioGenerationStartRequest'].includes(call.kind)), false);
  screen.unmount();
});

test('Tests Write without Environments Read creates a manual run and disables automatic types', async () => {
  const manual = { caseId: 'manual-case', title: 'Manual case', kind: 'manual', status: 'approved', priority: 'medium' };
  await mount(testAccess(), {
    projectStudioSettingsGetRequest: { settings: { tags: [] } },
    projectStudioCasesListRequest: { cases: [manual], total: 1 },
    projectStudioRunsListRequest: { runs: [], total: 0 },
    projectStudioSuitesListRequest: { suites: [{ suiteId: 'manual-suite', name: 'Manual suite', caseCount: 1 }] },
    projectStudioSuiteGetRequest: { cases: [manual] },
    projectStudioRunnersListRequest: { runners: [] },
    projectStudioRunCreateRequest: { runId: 'manual-run', runNo: 1 },
    projectStudioRunGetRequest: { run: { runId: 'manual-run', runNo: 1, runType: 'manual', name: 'Manual accessible run', status: 'running', total: 1, pending: 1 }, items: [] },
  });
  await openTestsView('runs');
  document.querySelector('#ps-runs-new').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 8; i += 1) await flush();
  const type = document.querySelector('#ps-run-type');
  assert.equal(type.value, 'manual');
  for (const value of ['auto', 'perf']) assert.ok(type.querySelector(`[data-value="${value}"]`).disabled);
  type.dispatchEvent(new CustomEvent('change', { detail: { value: 'auto' }, bubbles: true }));
  assert.equal(type.value, 'manual');
  document.querySelector('#ps-run-name').value = 'Manual accessible run';
  document.querySelector('#ps-run-suite').value = 'manual-suite';
  document.querySelector('#ps-run-suite').dispatchEvent(new CustomEvent('change', { detail: { value: 'manual-suite' }, bubbles: true }));
  await flush();
  document.querySelector('tf-window [data-action="create"]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  assert.equal(calls.filter((call) => call.kind === 'projectStudioRunCreateRequest').length, 1);
  assert.equal(calls.some((call) => ['projectStudioEnvironmentsListRequest', 'projectStudioRunStartAutoRequest'].includes(call.kind)), false);
  screen.unmount();
});

test('Tests Admin without Environments Read preserves automatic schedules and can create manual ones', async () => {
  const schedules = [
    { scheduleId: 'auto', name: 'Automatic schedule', runType: 'auto', enabled: false, scheduleKind: 'interval', scheduleExpr: '10m', timezone: 'UTC', environmentStatus: 'approved' },
    { scheduleId: 'perf', name: 'Performance schedule', runType: 'perf', enabled: true, scheduleKind: 'interval', scheduleExpr: '10m', timezone: 'UTC', environmentStatus: 'approved' },
    { scheduleId: 'manual', name: 'Manual schedule', runType: 'manual', enabled: true, scheduleKind: 'interval', scheduleExpr: '10m', timezone: 'UTC' },
  ];
  await mount(testAccess('admin'), {
    projectStudioSettingsGetRequest: { settings: { tags: [] } },
    projectStudioCasesListRequest: { cases: [], total: 0 },
    projectStudioSchedulesListRequest: { schedules, serverTimezone: 'UTC' },
    projectStudioSuitesListRequest: { suites: [{ suiteId: 'manual-suite', name: 'Manual suite', caseCount: 1 }] },
    projectStudioRunnersListRequest: { runners: [] },
    projectStudioScheduleSetEnabledRequest: { ok: true },
    projectStudioScheduleSaveRequest: { scheduleId: 'created-manual', nextRunAt: '2030-01-01T00:00:00Z' },
  });
  await openTestsView('schedules');
  const table = document.querySelector('#ps-sch-table');
  const actions = (id) => table.rowActions(table.rows.find((row) => row._id === id));
  const auto = actions('auto');
  const perf = actions('perf');
  const manual = actions('manual');
  assert.ok(auto.querySelector('tf-toggle').hasAttribute('disabled'));
  assert.ok(!perf.querySelector('tf-toggle').hasAttribute('disabled'), 'an administrator may stop an automatic schedule');
  assert.ok(!auto.querySelector('[icon="play"]') && !perf.querySelector('[icon="play"]'));
  assert.ok(manual.querySelector('[icon="play"]'));
  auto.querySelector('tf-toggle').dispatchEvent(new CustomEvent('change', { detail: { checked: true }, bubbles: true }));
  await flush();
  assert.equal(calls.some((call) => call.kind === 'projectStudioScheduleSetEnabledRequest'), false);
  document.body.appendChild(auto);
  auto.querySelector('[icon="edit"]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 8; i += 1) await flush();
  assert.equal(document.querySelector('#ps-sch-runtype').value, 'auto', 'opening an existing definition never converts it');
  assert.ok(document.querySelector('tf-window [data-action="save"]').hasAttribute('disabled'));
  document.querySelector('tf-window [data-action="save"]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await flush();
  assert.equal(calls.some((call) => call.kind === 'projectStudioScheduleSaveRequest'), false);
  document.querySelector('tf-window [data-action="cancel"]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 280));
  document.querySelector('#ps-sch-new').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const type = document.querySelector('#ps-sch-runtype');
  assert.equal(type.value, 'manual');
  for (const value of ['auto', 'perf']) assert.ok(type.querySelector(`[data-value="${value}"]`).disabled);
  document.querySelector('#ps-sch-name').value = 'New manual schedule';
  document.querySelector('#ps-sch-suite').value = 'manual-suite';
  document.querySelector('#ps-sch-suite').dispatchEvent(new CustomEvent('change', { detail: { value: 'manual-suite' }, bubbles: true }));
  document.querySelector('#ps-sch-expr-interval').value = '10m';
  document.querySelector('tf-window [data-action="save"]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const saved = calls.filter((call) => call.kind === 'projectStudioScheduleSaveRequest');
  assert.equal(saved.length, 1);
  assert.equal(saved[0].payload.runType, 'manual');
  assert.equal(saved[0].payload.environmentId, '');
  assert.equal(calls.some((call) => call.kind === 'projectStudioEnvironmentsListRequest'), false);
  screen.unmount();
});

test('Tests Write can run a manual schedule without offering administrative changes or automatic execution', async () => {
  await mount(testAccess(), {
    projectStudioSettingsGetRequest: { settings: { tags: [] } },
    projectStudioCasesListRequest: { cases: [], total: 0 },
    projectStudioSchedulesListRequest: { schedules: [
      { scheduleId: 'auto', name: 'Automatic schedule', runType: 'auto', enabled: true, scheduleKind: 'interval', scheduleExpr: '10m', environmentStatus: 'approved' },
      { scheduleId: 'manual', name: 'Manual schedule', runType: 'manual', enabled: true, scheduleKind: 'interval', scheduleExpr: '10m' },
    ], serverTimezone: 'UTC' },
    projectStudioScheduleRunNowRequest: { outcome: 'skipped', reason: 'disabled' },
  });
  await openTestsView('schedules');
  assert.ok(!document.querySelector('#ps-sch-new'));
  const table = document.querySelector('#ps-sch-table');
  const auto = table.rowActions(table.rows[0]);
  const manual = table.rowActions(table.rows[1]);
  assert.ok(!auto.querySelector('[icon="play"]'));
  assert.ok(!manual.querySelector('tf-toggle, [icon="edit"], [icon="trash"]'));
  const run = manual.querySelector('[icon="play"]');
  assert.ok(run);
  run.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await flush();
  assert.deepEqual(calls.filter((call) => call.kind === 'projectStudioScheduleRunNowRequest').map((call) => call.payload), [{ projectId: 'p0-project', scheduleId: 'manual' }]);
  screen.unmount();
});

test('Board Write without Tasks Read moves cards and saves only status from the card window', async () => {
  const task = { taskId: 'task-one', taskNo: 1, taskKey: 'WF-1', taskType: 'technical', title: 'Keep all task content', priority: 'medium', status: 'todo', assignedTo: '', dueDate: '', linksJson: '[]', parentTaskId: null, archivedAt: null, createdBy: 'author', commentCount: 0 };
  const boardAccess = access({ areas: PROJECT_AREAS.map((area) => ({ area, enabled: true, level: area === 'board' ? 'write' : 'none' })) });
  await mount(boardAccess, {
    projectStudioTasksListRequest: { tasks: [task], total: 1 },
    projectStudioTaskGetRequest: { detail: { info: task, descriptionMd: 'Existing content', attachments: [], comments: [], taskLinks: [], events: [], eventsHasMore: false, statusDurations: [], handoverCommentId: null } },
    projectStudioTaskStatusSetRequest: { ok: true },
  });
  document.querySelector('#ps-project-tabs').dispatchEvent(new CustomEvent('change', { detail: { value: 'tasks' }, bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const board = document.querySelector('tf-kanban');
  assert.ok(board, 'a board reader is sent to Board rather than the unavailable List');
  assert.equal(board.readOnly, false);
  board.dispatchEvent(new CustomEvent('card-move', { detail: { cardId: 'task-one', to: 'in_progress' }, bubbles: true }));
  await flush();
  board.dispatchEvent(new CustomEvent('card-open', { detail: { cardId: 'task-one' }, bubbles: true }));
  for (let i = 0; i < 4; i += 1) await flush();
  assert.ok(document.querySelector('#ps-task-title').hasAttribute('readonly'));
  assert.equal(document.querySelector('#ps-task-desc'), null);
  assert.equal(document.querySelector('#ps-task-desc-preview').textContent.trim(), 'Existing content');
  assert.ok(!document.querySelector('#ps-task-status').hasAttribute('disabled'));
  document.querySelector('#ps-task-status').dispatchEvent(new CustomEvent('change', { detail: { value: 'review' }, bubbles: true }));
  document.querySelector('tf-window [data-action="save"]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 4; i += 1) await flush();
  assert.deepEqual(calls.filter((call) => call.kind === 'projectStudioTaskStatusSetRequest').map((call) => call.payload), [
    { projectId: 'p0-project', taskId: 'task-one', status: 'in_progress' },
    { projectId: 'p0-project', taskId: 'task-one', status: 'review' },
  ]);
  assert.equal(calls.some((call) => call.kind === 'projectStudioTaskSaveRequest'), false);
  screen.unmount();
});

test('Tasks Write with Board None keeps the list and refuses the unavailable board mode', async () => {
  await mount(access({ areas: PROJECT_AREAS.map((area) => ({ area, enabled: true, level: area === 'tasks' ? 'write' : 'none' })) }), {
    projectStudioTasksListRequest: { tasks: [], total: 0 },
  });
  document.querySelector('#ps-project-tabs').dispatchEvent(new CustomEvent('change', { detail: { value: 'tasks' }, bubbles: true }));
  for (let i = 0; i < 4; i += 1) await flush();
  const mode = document.querySelector('#ps-tasks-mode');
  assert.equal(mode.value, 'list');
  assert.ok(!document.querySelector('tf-kanban'));
  mode.dispatchEvent(new CustomEvent('change', { detail: { value: 'board' }, bubbles: true }));
  for (let i = 0; i < 4; i += 1) await flush();
  assert.ok(!document.querySelector('tf-kanban'));
  screen.unmount();
});

test('task view preferences belong to the signed-in user and do not reuse the old project-only key', async () => {
  const reader = access({ areas: PROJECT_AREAS.map((area) => ({ area, enabled: true, level: ['tasks', 'board'].includes(area) ? 'read' : 'none' })) });
  const userA = new Uint8Array(16).fill(1);
  const userB = new Uint8Array(16).fill(2);
  localStorage.setItem('ps.tasks.view.p0-project', 'board');
  await mount(reader, {
    authMeRequest: { userId: userA, username: 'First person' },
    projectStudioTasksListRequest: { tasks: [], total: 0 },
  });
  await openTasks();
  assert.equal(document.querySelector('#ps-tasks-mode').getAttribute('value'), 'list');
  document.querySelector('#ps-tasks-mode').dispatchEvent(new CustomEvent('change', { detail: { value: 'board' }, bubbles: true }));
  await settle();
  assert.equal(localStorage.getItem(`ps.tasks.view.${'01'.repeat(16)}.p0-project`), 'board');
  await mount(reader, {
    authMeRequest: { userId: userB, username: 'Second person' },
    projectStudioTasksListRequest: { tasks: [], total: 0 },
  });
  await openTasks();
  assert.equal(document.querySelector('#ps-tasks-mode').getAttribute('value'), 'list');
  await mount(reader, {
    authMeRequest: { userId: userA, username: 'First person' },
    projectStudioTasksListRequest: { tasks: [], total: 0 },
  });
  await openTasks();
  assert.equal(document.querySelector('#ps-tasks-mode').getAttribute('value'), 'board');
});

test('editing a function saves one complete row of the matrix in one mutation', async () => {
  await mount(access({ projectAdmin: true, canManageMembers: true }), { projectStudioFunctionSaveRequest: { ok: true } });
  document.querySelector('[data-goto-members]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const button = document.querySelector('[data-function-menu="3"]');
  button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  const item = [...document.querySelectorAll('tf-menu tf-menu-item')].find((row) => row.getAttribute('label') === I18n.t('project_studio.function_edit'));
  item.closest('tf-menu').dispatchEvent(new CustomEvent('action', { detail: { action: item.getAttribute('action') }, bubbles: true }));
  document.querySelector('[data-function-area="tasks"]').value = 'admin';
  document.querySelector('tf-window [data-action="save"]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const saves = calls.filter((call) => call.kind === 'projectStudioFunctionSaveRequest');
  assert.equal(saves.length, 1);
  assert.equal(saves[0].payload.function.name, 'Engineering');
  assert.equal(saves[0].payload.function.grants.length, PROJECT_AREAS.length);
  assert.deepEqual(saves[0].payload.function.grants.find((grant) => grant.area === 'tasks'), { area: 'tasks', level: 'admin' });
  assert.deepEqual(saves[0].payload.function.grants.find((grant) => grant.area === 'knowledge'), { area: 'knowledge', level: 'read' });
  screen.unmount();
});

function tasksAccess(level = 'write', board = 'read') {
  return access({ areas: PROJECT_AREAS.map((area) => ({ area, enabled: true, level: area === 'tasks' ? level : area === 'board' ? board : 'none' })) });
}

function taskFixture(overrides = {}) {
  return { projectId: 'p0-project', projectName: 'Workflow', taskTypeName: 'Technical', resolution: null, resolutionReason: null, taskId: 'task-one', taskNo: 7, taskKey: 'WF-7', taskType: 'technical', title: 'Persistent task', descriptionMd: '', severity: '', priority: 'medium', status: 'in_progress', assignedTo: '', assignedToName: '', dueDate: '', parentTaskId: null, linksJson: '[]', commentCount: 0, createdBy: 'author', createdByName: 'Author', createdAt: '2026-10-01T10:00:00Z', updatedAt: '2026-10-01T10:00:00Z', archivedAt: null, ...overrides };
}

function detailFixture(info, overrides = {}) {
  return { info, descriptionMd: '**Actual description**', attachments: [], comments: [], taskLinks: [], events: [], eventsHasMore: false, statusDurations: [], handoverCommentId: null, ...overrides };
}

async function openTasks() {
  document.querySelector('#ps-project-tabs').dispatchEvent(new CustomEvent('change', { detail: { value: 'tasks' }, bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();
}

async function openTaskCard() {
  await openTasks();
  const table = document.querySelector('#ps-tasks-table');
  table.dispatchEvent(new CustomEvent('row-click', { detail: { row: table.rows[0] }, bubbles: true }));
  for (let i = 0; i < 8; i += 1) await flush();
  assert.ok(document.querySelector('#ps-task-title'));
}

const click = (element) => { assert.ok(element); element.dispatchEvent(new MouseEvent('click', { bubbles: true })); };
const settle = async () => { for (let i = 0; i < 8; i += 1) await flush(); };

function menuAction(label) {
  const item = [...document.querySelectorAll('tf-menu tf-menu-item')].find((node) => node.getAttribute('label') === label);
  assert.ok(item, `menu action ${label}`);
  item.closest('tf-menu').dispatchEvent(new CustomEvent('action', { detail: { action: item.getAttribute('action') }, bubbles: true }));
}

for (const taskType of ['technical', 'subtask']) test(`${taskType} creation preserves the chosen Epic after input blur and Save`, async () => {
  const epic = taskFixture({ taskId: 'epic-id', taskKey: 'WF-1', taskType: 'epic', title: 'Epic parent' });
  await mount(tasksAccess(), { projectStudioTasksListRequest: { tasks: [epic], total: 1 } });
  click(document.querySelector('[data-new-task]')); await settle();
  const title = document.querySelector('#ps-task-title input');
  title.value = 'Task with a persisted parent'; title.dispatchEvent(new Event('input', { bubbles: true }));
  const type = document.querySelector('#ps-task-type select');
  type.value = taskType; type.dispatchEvent(new Event('change', { bubbles: true })); await settle();
  const save = document.querySelector('tf-window tf-button[data-action="save"]');
  if (taskType === 'subtask') {
    click(save); await settle();
    assert.equal(calls.some((call) => call.kind === 'projectStudioTaskSaveRequest'), false, 'a subtask without a selected parent is rejected');
    assert.match(document.querySelector('[data-form-error]').textContent, /parent/i);
  }
  const parent = document.querySelector('#ps-task-parent');
  const input = parent.querySelector('input');
  input.focus(); input.value = 'WF-1'; input.dispatchEvent(new Event('input', { bubbles: true })); await settle();
  parent.querySelector('[role="option"]').dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
  input.blur(); input.dispatchEvent(new Event('change', { bubbles: true }));
  click(save); await settle();
  const writes = calls.filter((call) => call.kind === 'projectStudioTaskSaveRequest');
  assert.equal(writes.length, 1);
  assert.equal(writes[0].payload.parentTaskId, 'epic-id');
  assert.equal(writes[0].payload.taskType, taskType);
});

test('Tasks Admin with Settings None creates, edits and deactivates a custom type from Tasks', async () => {
  const types = taskTypes();
  await mount(tasksAccess('admin'), {
    projectStudioTasksListRequest: { tasks: [], total: 0 },
    projectStudioTaskTypesListRequest: () => ({ types }),
    projectStudioTaskTypeSaveRequest: (input) => {
      const type = { typeId: input.typeId, name: input.name, description: input.description, sortOrder: input.sortOrder, active: input.active, builtIn: false };
      const existing = types.findIndex((item) => item.typeId === type.typeId);
      if (existing >= 0) types[existing] = type; else types.push(type);
      return { taskType: type };
    },
  });
  assert.equal(document.querySelector('[data-goto-settings]'), null);
  await openTasks(); click(document.querySelector('#ps-tasks-types')); await settle();
  const catalogue = document.querySelector('.ps-task-types');
  assert.ok(catalogue);
  const table = catalogue.querySelector('tf-table');
  assert.equal(table.rowActions(table.rows[0]), null, 'built-in definitions are immutable');
  click(catalogue.querySelector('[data-new-type]'));
  document.querySelector('#ps-type-id').value = 'customer_review';
  document.querySelector('#ps-type-name').value = 'Customer <review>';
  document.querySelector('#ps-type-desc').value = 'Actual per-project description';
  click(document.querySelector('#ps-type-name').closest('tf-window').querySelector('[data-action="save"]'));
  await settle(); await new Promise((resolve) => setTimeout(resolve, 260));
  let row = catalogue.querySelector('tf-table').rows.find((entry) => entry._id === 'customer_review');
  assert.match(row.name, /Customer &lt;review&gt;/);
  let action = catalogue.querySelector('tf-table').rowActions(row); document.body.appendChild(action); click(action); menuAction('Edit');
  assert.ok(document.querySelector('#ps-type-id').hasAttribute('readonly'));
  document.querySelector('#ps-type-name').value = 'Release review';
  click(document.querySelector('#ps-type-name').closest('tf-window').querySelector('[data-action="save"]'));
  await settle(); await new Promise((resolve) => setTimeout(resolve, 260));
  row = catalogue.querySelector('tf-table').rows.find((entry) => entry._id === 'customer_review');
  action = catalogue.querySelector('tf-table').rowActions(row); document.body.appendChild(action); click(action); menuAction('Deactivate'); await settle();
  assert.equal(types.find((entry) => entry.typeId === 'customer_review').active, false);
  assert.deepEqual(calls.filter((call) => call.kind === 'projectStudioTaskTypeSaveRequest').map((call) => [call.payload.typeId, call.payload.name, call.payload.active]), [['customer_review', 'Customer <review>', true], ['customer_review', 'Release review', true], ['customer_review', 'Release review', false]]);
});

test('a task card renders inbound relationships, escaped history, historical attachments and cursor pages', async () => {
  const info = taskFixture();
  const attachment = { sha256: 'a'.repeat(64), name: 'Previous recording.avi', size_bytes: 90000000, mime: 'video/x-msvideo' };
  const events = [{ eventId: 8, taskId: info.taskId, at: '2026-10-01T12:00:00Z', actorKind: 'user', actorId: 'author', kind: 'title', beforeJson: '"Original"', afterJson: '"<img src=x onerror=bad()>"' },
    { eventId: 7, taskId: info.taskId, at: '2026-10-01T11:00:00Z', actorKind: 'user', actorId: 'author', kind: 'attachments_json', beforeJson: JSON.stringify(JSON.stringify([attachment])), afterJson: '"[]"' }];
  const detail = detailFixture(info, { events, eventsHasMore: true, taskLinks: [{ linkId: 11, sourceTaskId: 'epic', targetTaskId: info.taskId, kind: 'fs', lagDays: 2, counterpartyTaskKey: 'WF-1', counterpartyTaskTitle: 'Prior task' }], statusDurations: [{ status: 'todo', enteredAt: '2026-10-01T10:00:00Z', leftAt: '2026-10-01T11:00:00Z', seconds: 3600 }, { status: 'in_progress', enteredAt: '2026-10-01T11:00:00Z', leftAt: null, seconds: 120 }] });
  await mount(tasksAccess('read'), { projectStudioTasksListRequest: { tasks: [info], total: 1 }, projectStudioTaskGetRequest: { detail }, projectStudioTaskEventsRequest: { events: [{ ...events[0], eventId: 6, kind: 'created', beforeJson: 'null', afterJson: '{"title":"Initial"}' }], hasMore: false } });
  await openTaskCard();
  assert.match(document.querySelector('#ps-task-relations').textContent, /WF-1.*Prior task/);
  assert.match(document.querySelector('#ps-task-relations').textContent, /successor/);
  assert.match(document.querySelector('#ps-task-durations').textContent, /1h 0m/);
  const history = document.querySelector('#ps-task-history');
  assert.equal(history.querySelector('img'), null);
  assert.match(history.textContent, /<img src=x onerror=bad\(\)>/);
  assert.match(history.querySelector('[data-history-attachment]').textContent, /Previous recording/);
  click(document.querySelector('#ps-task-history-more')); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioTaskEventsRequest').payload, { projectId: 'p0-project', taskId: info.taskId, beforeId: 7, limit: 50 });
  assert.equal(history.entries.length, 3);
  assert.equal(document.querySelector('#ps-task-history-more').hidden, true);
});

test('attachment gallery actions render localized preview and remove labels in every language', async () => {
  const originalAction = ApiBinary.action;
  ApiBinary.action = async (kind, payload) => {
    assert.equal(kind, 'mePreferencesUpdateRequest');
    assert.ok(['pl', 'en', 'de', 'es', 'fr'].includes(payload.language));
    return { ok: true };
  };
  try {
    for (const language of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const info = taskFixture();
      const attachment = { sha256: 'c'.repeat(64), name: 'Actual original.avi', sizeBytes: 124709766, mime: 'video/avi' };
      await mount(tasksAccess(), { projectStudioTasksListRequest: { tasks: [info], total: 1 }, projectStudioTaskGetRequest: { detail: detailFixture(info, { attachments: [attachment] }) } });
      await openTaskCard();
      const preview = document.querySelector('#ps-task-atts [data-att-preview]');
      const remove = document.querySelector('#ps-task-atts [data-att-remove]');
      assert.equal(preview.textContent, I18n.t('project_studio.kb_preview'));
      assert.equal(preview.title, I18n.t('project_studio.kb_preview'));
      assert.equal(remove.title, I18n.t('project_studio.attachment_remove'));
      assert.equal(preview.textContent.includes('project_studio.'), false);
      assert.equal(remove.title.includes('project_studio.'), false);
      click(remove);
      assert.equal(document.querySelector('#ps-task-atts [data-att-preview]'), null);
      click(document.querySelector('#ps-task-title').closest('tf-window').querySelector('[data-action="save"]')); await settle();
      assert.equal(calls.find((call) => call.kind === 'projectStudioTaskSaveRequest').payload.attachmentsJson, '[]');
    }
  } finally { await I18n.setLanguage('en'); ApiBinary.action = originalAction; }
});

test('history resolves authorized task labels asynchronously and keeps denied references private', async () => {
  const info = taskFixture();
  let release;
  const resolved = new Promise((resolve) => { release = resolve; });
  const deniedId = 'private-task-id';
  const event = (id, kind, before, after) => ({ eventId: id, taskId: info.taskId, at: '2026-10-01T11:00:00Z', actorKind: 'user', actorId: 'removed-person-id', kind, beforeJson: JSON.stringify(before), afterJson: JSON.stringify(after) });
  const events = [event(9, 'parent_task_id', 'previous-parent-id', null),
    event(8, 'link_deleted', { link_id: 4, source_task_id: info.taskId, target_task_id: deniedId, kind: 'related', lag_days: 0 }, null),
    event(7, 'created', null, { task_type: 'release_review', priority: 'high', status: 'review', parent_task_id: 'previous-parent-id' })];
  await mount(tasksAccess('read'), {
    projectStudioTasksListRequest: { tasks: [info], total: 1 },
    projectStudioTaskTypesListRequest: { types: [...taskTypes(), { typeId: 'release_review', name: 'Release review', description: '', active: false, builtIn: false, sortOrder: 70 }] },
    projectStudioTaskGetRequest: async ({ taskId, projectId }) => {
      assert.equal(projectId, 'p0-project');
      if (taskId === info.taskId) return { detail: detailFixture(info, { events }) };
      if (taskId === 'previous-parent-id') { await resolved; return { detail: detailFixture(taskFixture({ taskId: taskId, taskKey: 'WF-1', title: '<Saved parent>' })) }; }
      if (taskId === deniedId) throw new Error('Project access was denied');
      throw new Error(`unexpected task ${taskId}`);
    },
  });
  await openTaskCard();
  const history = document.querySelector('#ps-task-history');
  assert.equal(history.textContent.includes('previous-parent-id'), false);
  assert.equal(history.textContent.includes(deniedId), false);
  assert.match(history.textContent, /Record unavailable or removed/);
  release(); await settle();
  assert.match(history.textContent, /WF-1 · <Saved parent>/);
  assert.match(history.textContent, /Release review/);
  assert.match(history.textContent, /High/);
  assert.ok(history.textContent.includes(I18n.t('project_studio.task_status_review')));
  assert.match(history.textContent, /Related/);
  assert.equal(history.textContent.includes('removed-person-id'), false);
  assert.equal(history.textContent.includes('release_review'), false);
  assert.equal(history.textContent.includes('Relationship identifier'), false);
  assert.equal(history.querySelector('saved'), null);
  assert.equal(calls.filter((call) => call.kind === 'projectStudioTaskGetRequest' && call.payload.taskId === deniedId).length, 1);
});

test('a full history page resolves every before and after reference with bounded request concurrency', async () => {
  const info = taskFixture();
  const events = Array.from({ length: 50 }, (_, i) => ({ eventId: 50 - i, taskId: info.taskId, at: '2026-10-01T11:00:00Z', actorKind: 'system', actorId: '', kind: 'parent_task_id', beforeJson: JSON.stringify(`previous-${i}`), afterJson: JSON.stringify(`current-${i}`) }));
  let active = 0, peak = 0, resolved = 0;
  await mount(tasksAccess('read'), {
    projectStudioTasksListRequest: { tasks: [info], total: 1 },
    projectStudioTaskGetRequest: async ({ projectId, taskId }) => {
      assert.equal(projectId, 'p0-project');
      if (taskId === info.taskId) return { detail: detailFixture(info, { events }) };
      active += 1; peak = Math.max(peak, active); await flush(); active -= 1; resolved += 1;
      return { detail: detailFixture(taskFixture({ taskId: taskId, taskKey: `WF-${taskId}`, title: 'Authorized historical parent' })) };
    },
  });
  await openTaskCard();
  for (let i = 0; resolved < 100 && i < 100; i += 1) await flush();
  assert.equal(resolved, 100); assert.ok(peak > 1 && peak <= 5);
  const history = document.querySelector('#ps-task-history');
  assert.match(history.textContent, /WF-previous-49 · Authorized historical parent/);
  assert.match(history.textContent, /WF-current-49 · Authorized historical parent/);
  assert.equal(history.textContent.includes('Record unavailable or removed'), false);
  assert.equal(calls.filter((call) => call.kind === 'projectStudioTaskGetRequest').length, 101);
});

test('history does not reuse cached or historical project names after a current read denial', async () => {
  const info = taskFixture();
  const current = treeProject('p0-project', 'Current <authorized> project', tasksAccess('read'));
  const stale = treeProject('old-private-project', 'Cached private name', tasksAccess('read'));
  const events = [{ eventId: 7, taskId: info.taskId, at: '2026-10-01T11:00:00Z', actorKind: 'system', actorId: '', kind: 'transferred',
    beforeJson: JSON.stringify({ project_id: stale.projectId, project_name: 'Historical private name', task_key: 'OLD-7' }),
    afterJson: JSON.stringify({ project_id: current.projectId, project_name: 'Historical public name', task_key: 'WF-7', operation_id: 'private-operation-uuid' }) }];
  await mount(current.access, treeFixtures([current, stale], {
    projectStudioProjectGetRequest: ({ projectId }) => { if (projectId === current.projectId) return { project: current }; throw new Error('Current project access was denied'); },
    projectStudioTasksListRequest: { tasks: [info], total: 1 },
    projectStudioTaskGetRequest: { detail: detailFixture({ ...info, projectName: current.name }, { events }) },
  }));
  await openTaskCard(); await settle();
  const history = document.querySelector('#ps-task-history');
  assert.ok(history.textContent.includes(current.name));
  assert.ok(history.textContent.includes(I18n.t('project_studio.task_history_unavailable_record')));
  for (const privateText of [stale.projectId, stale.name, 'Historical private name', 'Historical public name', 'private-operation-uuid']) assert.equal(history.textContent.includes(privateText), false);
  assert.equal(history.querySelector('authorized'), null);
  assert.equal(calls.filter(call => call.kind === 'projectStudioProjectGetRequest' && call.payload.projectId === stale.projectId).length, 1);
});

test('history resolves one hundred task and project references with shared concurrency and current project authorization', async () => {
  const info = taskFixture();
  const events = Array.from({ length: 50 }, (_, index) => ({ eventId: 50 - index, taskId: info.taskId, at: '2026-10-01T11:00:00Z', actorKind: 'system', actorId: '', kind: 'link_created', beforeJson: 'null', afterJson: JSON.stringify({ source_task_id: info.taskId, target_task_id: `task-ref-${index}`, source_project_id: info.projectId, target_project_id: `project-ref-${index}`, relation_id: `relation-ref-${index}`, kind: 'related', lag_days: 0 }) }));
  let active = 0, peak = 0, resolved = 0;
  const resolve = async (id, create) => {
    active += 1; peak = Math.max(peak, active); await flush(); active -= 1; resolved += 1;
    if (id.endsWith('-49')) throw new Error('Current project read was denied');
    return create();
  };
  await mount(tasksAccess('read'), {
    projectStudioTasksListRequest: { tasks: [info], total: 1 },
    projectStudioProjectGetRequest: ({ projectId }) => projectId === info.projectId ? { project: projectFixture(tasksAccess('read')) } : resolve(projectId, () => ({ project: projectFixture(tasksAccess('read'), { projectId: projectId, name: `Current project <${projectId.slice(12)}>` }) })),
    projectStudioTaskGetRequest: ({ taskId }) => taskId === info.taskId ? { detail: detailFixture(info, { events }) } : resolve(taskId, () => ({ detail: detailFixture(taskFixture({ taskId: taskId, taskKey: `CH-${taskId.slice(9)}`, title: 'Currently authorized task', projectId: `project-ref-${taskId.slice(9)}`, projectName: `Current project <${taskId.slice(9)}>` })) })),
  });
  await openTaskCard();
  for (let index = 0; resolved < 100 && index < 100; index += 1) await flush();
  assert.equal(resolved, 100); assert.ok(peak > 1 && peak <= 5);
  const history = document.querySelector('#ps-task-history');
  assert.ok(history.textContent.includes('Current project <48>'));
  assert.ok(history.textContent.includes('CH-48 · Currently authorized task'));
  assert.ok(history.textContent.includes(I18n.t('project_studio.task_history_unavailable_record')));
  for (const internal of ['project-ref-', 'task-ref-', 'relation-ref-']) assert.equal(history.textContent.includes(internal), false);
  assert.equal(history.querySelector('current'), null);
  assert.equal(calls.filter(call => call.kind === 'projectStudioProjectGetRequest' && call.payload.projectId.startsWith('project-ref-')).length, 50);
});

test('comment add and edit retain structural mention IDs with equal display names', async () => {
  const info = taskFixture();
  const actor = '0'.repeat(32);
  const members = [{ userId: 'reader-one', displayName: 'Same name', active: true, access: tasksAccess('read') }, { userId: 'reader-two', displayName: 'Same name', active: true, access: tasksAccess('read') }];
  const detail = detailFixture(info);
  await mount(tasksAccess(), { projectStudioTasksListRequest: { tasks: [info], total: 1 }, projectStudioMembersListRequest: { members }, projectStudioTaskGetRequest: () => ({ detail }),
    projectStudioTaskCommentAddRequest: (input) => { detail.comments = [{ commentId: 'comment', authorUserId: actor, authorName: 'Creator', bodyMd: input.bodyMd, mentionUserIds: input.mentionUserIds, createdAt: '2026-10-01T12:00:00Z', editedAt: null }]; return { comment: detail.comments[0] }; },
    projectStudioTaskCommentEditRequest: (input) => { Object.assign(detail.comments[0], { bodyMd: input.bodyMd, mentionUserIds: input.mentionUserIds, editedAt: '2026-10-01T12:05:00Z' }); return { comment: detail.comments[0] }; },
  });
  await openTaskCard();
  document.querySelector('#ps-task-comment-input').value = '**Please review**';
  document.querySelector('#ps-task-mentions').value = ['reader-two'];
  click(document.querySelector('#ps-task-comment-send')); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioTaskCommentAddRequest').payload.mentionUserIds, ['reader-two']);
  assert.match(document.querySelector('#ps-task-comments strong').textContent, /Please review/);
  click(document.querySelector('[data-comment-edit]'));
  const picker = document.querySelector('#ps-comment-edit-mentions');
  assert.deepEqual(picker.value, ['reader-two']);
  picker.value = ['reader-one', 'reader-two'];
  document.querySelector('#ps-comment-edit-body').value = 'Please review both';
  click(picker.closest('tf-window').querySelector('[data-action="save"]')); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioTaskCommentEditRequest').payload, { projectId: 'p0-project', commentId: 'comment', bodyMd: 'Please review both', mentionUserIds: ['reader-one', 'reader-two'] });
  assert.match(document.querySelector('#ps-task-comments').textContent, /edited/);
});

test('self-assigned readers hand over atomically with a mandatory note and no future-feature switches', async () => {
  const actor = '0'.repeat(32);
  const info = taskFixture({ assignedTo: actor });
  const detail = detailFixture(info);
  const successor = { userId: 'successor', displayName: 'Successor', active: true, access: tasksAccess() };
  await mount(tasksAccess('read'), { projectStudioTasksListRequest: { tasks: [info], total: 1 }, projectStudioTaskGetRequest: { detail }, projectStudioMembersListRequest: { members: [successor] }, projectStudioTaskHandoverRequest: { ok: true, eventIds: [20, 21], commentId: 'note' } });
  await openTaskCard(); click(document.querySelector('#ps-task-handover'));
  const win = document.querySelector('.tf-act-window');
  assert.ok(win);
  assert.equal(win.querySelector('tf-checkbox'), null);
  win.querySelector('tf-person-picker').value = 'successor';
  click(win.querySelector('[data-act="submit"]')); await settle();
  assert.equal(calls.some((call) => call.kind === 'projectStudioTaskHandoverRequest'), false);
  win.querySelector('tf-textarea').value = 'Continue the verified reproduction steps.';
  click(win.querySelector('[data-act="submit"]')); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioTaskHandoverRequest').payload, { projectId: 'p0-project', taskId: 'task-one', assignedTo: 'successor', noteMd: 'Continue the verified reproduction steps.', mentionUserIds: ['successor'] });
  assert.equal(calls.some((call) => ['projectStudioTaskSaveRequest', 'projectStudioTaskCommentAddRequest'].includes(call.kind)), false);
});

test('handover notes are pinned only for the current recipient and disappear after ordinary reassignment', async () => {
  const actor = '0'.repeat(32);
  const info = taskFixture({ assignedTo: actor });
  const detail = detailFixture(info, { handoverCommentId: 'note', comments: [{ commentId: 'note', bodyMd: '**Next steps**', authorUserId: 'giver', authorName: 'Giver', mentionUserIds: [actor], createdAt: '2026-10-01T12:00:00Z', editedAt: null }] });
  await mount(tasksAccess('read'), { projectStudioTasksListRequest: { tasks: [info], total: 1 }, projectStudioTaskGetRequest: { detail } });
  await openTaskCard();
  const pin = document.querySelector('#ps-task-handover-note');
  assert.equal(pin.hidden, false);
  assert.equal(pin.querySelector('strong').textContent, 'Next steps');
  screen.unmount();
  info.assignedTo = 'other-person'; detail.handoverCommentId = null;
  await mount(tasksAccess('read'), { projectStudioTasksListRequest: { tasks: [info], total: 1 }, projectStudioTaskGetRequest: { detail } });
  await openTaskCard();
  assert.equal(document.querySelector('#ps-task-handover-note').hidden, true);
});

test('archived tasks are discoverable and immutable while explicit restore and archive undo use real mutations', async () => {
  const info = taskFixture({ archivedAt: '2026-10-01T12:00:00Z' });
  await mount(tasksAccess(), { projectStudioTasksListRequest: (input) => ({ tasks: input.includeArchived ? [info] : [], total: input.includeArchived ? 1 : 0 }), projectStudioTaskGetRequest: { detail: detailFixture(info) }, projectStudioTaskArchiveRequest: { ok: true, eventId: 30 } });
  await openTasks();
  assert.equal(document.querySelector('#ps-tasks-table'), null);
  document.querySelector('#ps-tasks-f-archived').dispatchEvent(new CustomEvent('change', { detail: { checked: true }, bubbles: true })); await settle();
  const table = document.querySelector('#ps-tasks-table');
  assert.equal(table.rows[0].no, 'WF-7');
  table.dispatchEvent(new CustomEvent('row-click', { detail: { row: table.rows[0] }, bubbles: true })); await settle();
  assert.ok(document.querySelector('#ps-task-title').hasAttribute('readonly'));
  assert.equal(document.querySelector('#ps-task-handover'), null);
  const win = document.querySelector('#ps-task-title').closest('tf-window');
  assert.equal(win.querySelector('[data-action="save"]'), null);
  assert.match(win.querySelector('[data-action="archive"]').textContent, /Restore/);
  click(win.querySelector('[data-action="archive"]')); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioTaskArchiveRequest').payload, { projectId: 'p0-project', taskId: 'task-one', archived: false });
  click(document.querySelector('tf-toast tf-button')); await settle();
  assert.deepEqual(calls.filter((call) => call.kind === 'projectStudioTaskArchiveRequest').map((call) => call.payload.archived), [false, true]);
});

test('a task row opens its stable UUID without supplying partial origin-event metadata', async () => {
  const info = taskFixture();
  await mount(tasksAccess('read'), { projectStudioTasksListRequest: { tasks: [info], total: 1 }, projectStudioTaskGetRequest: { detail: detailFixture(info) } });
  await openTaskCard();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioTaskKeyResolveRequest').payload, { taskKey: null, taskId: info.taskId, originProjectId: null, originEventId: null });
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioTaskGetRequest').payload, { projectId: info.projectId, taskId: info.taskId });
});

test('a key deep link resolves the exact archived task and closing the card preserves the project route', async () => {
  const info = taskFixture({ archivedAt: '2026-10-01T12:00:00Z' });
  const original = { current: Router.current, currentParams: Router.currentParams, replaceParams: Router.replaceParams };
  let params = { instance: 'native-projects', projectId: 'p0-project', tab: 'tasks', taskKey: 'WF-7' };
  Router.current = () => 'projekty'; Router.currentParams = () => params; Router.replaceParams = (next) => { params = next; };
  try {
    await mount(tasksAccess('read'), { projectStudioTasksListRequest: (input) => ({ tasks: input.search === 'WF-7' ? [info, taskFixture({ taskId: 'other', taskKey: 'WF-70' })] : [], total: 2 }), projectStudioTaskGetRequest: { detail: detailFixture(info) } }, params);
    assert.ok(document.querySelector('#ps-task-title'));
    assert.equal(params.taskKey, 'WF-7');
    assert.equal(params.instance, 'native-projects');
    const resolve = calls.find((call) => call.kind === 'projectStudioTaskKeyResolveRequest');
    assert.deepEqual(resolve.payload, { taskKey: 'WF-7', taskId: null, originProjectId: null, originEventId: null });
    assert.equal(calls.some((call) => call.kind === 'projectStudioTasksListRequest' && call.payload.search === 'WF-7'), false);
    click(document.querySelector('#ps-task-title').closest('tf-window').querySelector('[data-action="cancel"]'));
    assert.equal(params.taskKey, null);
    assert.equal(params.projectId, 'p0-project');
    assert.equal(params.tab, 'tasks');
  } finally { Object.assign(Router, original); }
});

test('a status notification renders structured text and loads the exact older history event', async () => {
  const info = taskFixture();
  const event = (id) => ({ eventId: id, taskId: info.taskId, at: '2026-10-01T12:00:00Z', actorKind: 'user', actorId: 'author', kind: 'title', beforeJson: '"Previous"', afterJson: '"Current"' });
  const notification = { notificationId: 35, kind: 'task_status_changed', projectId: 'p0-project', projectName: 'Workflow', title: 'Raw server title', body: 'Raw server body', createdAt: '2026-10-01T12:00:00Z', readAt: null,
    linkJson: JSON.stringify({ project_id: 'p0-project', task_id: info.taskId, task_key: info.taskKey, task_title: '<img src=x onerror=bad()>', event_id: 80, from_status: 'todo', to_status: 'in_progress' }) };
  await mount(tasksAccess('read'), {
    projectStudioTasksListRequest: { tasks: [info], total: 1 },
    projectStudioTaskGetRequest: { detail: detailFixture(info, { events: [event(100)], eventsHasMore: true }) },
    projectStudioTaskEventsRequest: (input) => ({ events: [event(input.beforeId === 100 ? 90 : 80)], hasMore: input.beforeId === 100 }),
    projectStudioNotificationsListRequest: { notifications: [notification], unreadCount: 1, hasMore: false },
    projectStudioNotificationsMarkReadRequest: { ok: true },
  });
  click(document.querySelector('[data-bell]')); await settle();
  const item = document.querySelector('[data-notif="0"]');
  assert.equal(item.querySelector('img'), null);
  assert.match(item.textContent, /WF-7 · <img src=x onerror=bad\(\)>: To do → In progress/);
  assert.equal(item.textContent.includes('Raw server'), false);
  click(item); await settle();
  assert.deepEqual(calls.filter((call) => call.kind === 'projectStudioTaskEventsRequest').map((call) => call.payload.beforeId), [100, 90]);
  const target = document.querySelector('[data-task-event="80"]').closest('.tf-timeline-item');
  assert.ok(target.classList.contains('ps-task-target'));
  assert.equal(target.getAttribute('tabindex'), '-1');
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioNotificationsMarkReadRequest').payload, { notificationIds: [35] });
});

test('a mention notification opens and highlights its exact persisted comment', async () => {
  const info = taskFixture();
  const comments = ['older-comment', 'mentioned-comment'].map((commentId) => ({ commentId, authorUserId: 'author', authorName: 'Author', bodyMd: `**${commentId}**`, mentionUserIds: [], createdAt: '2026-10-01T12:00:00Z', editedAt: null }));
  const notification = { notificationId: 36, kind: 'task_mentioned', projectId: 'p0-project', projectName: 'Workflow', createdAt: '2026-10-01T12:00:00Z', readAt: '2026-10-01T12:01:00Z',
    linkJson: JSON.stringify({ project_id: 'p0-project', task_id: info.taskId, task_key: info.taskKey, task_title: info.title, comment_id: 'mentioned-comment', event_id: 9 }) };
  await mount(tasksAccess('read'), {
    projectStudioTasksListRequest: { tasks: [info], total: 1 }, projectStudioTaskGetRequest: { detail: detailFixture(info, { comments }) },
    projectStudioNotificationsListRequest: { notifications: [notification], unreadCount: 0, hasMore: false },
  });
  click(document.querySelector('[data-bell]')); await settle();
  click(document.querySelector('[data-notif="0"]')); await settle();
  assert.ok(document.querySelector('[data-task-comment="mentioned-comment"]').classList.contains('ps-task-target'));
  assert.equal(document.querySelector('[data-task-comment="older-comment"]').classList.contains('ps-task-target'), false);
  assert.equal(calls.find((call) => call.kind === 'projectStudioTaskGetRequest').payload.taskId, info.taskId);
  assert.equal(calls.some((call) => call.kind === 'projectStudioTaskEventsRequest'), false);
});

test('previous assignees see localized reassignment and removal notifications linked to their actual events', async () => {
  for (const [kind, toUserId, expectedTitle, eventId] of [
    ['task_reassigned', 'next-assignee', 'Task assigned to another person', 41],
    ['task_unassigned', '', 'Your task assignment was removed', 42],
  ]) {
    const info = taskFixture({ assignedTo: toUserId });
    const event = { eventId: eventId, taskId: info.taskId, at: '2026-10-01T12:00:00Z', actorKind: 'user', actorId: 'author', kind: kind === 'task_reassigned' ? 'reassigned' : 'unassigned', beforeJson: '"previous-assignee"', afterJson: JSON.stringify(toUserId) };
    const notification = { notificationId: eventId, kind, projectId: 'p0-project', projectName: 'Workflow', title: 'Unlocalized server title', body: 'Unlocalized server body', createdAt: '2026-10-01T12:00:00Z', readAt: '2026-10-01T12:01:00Z',
      linkJson: JSON.stringify({ project_id: 'p0-project', task_id: info.taskId, task_key: info.taskKey, task_title: '<b>Persistent task</b>', event_id: eventId, from_user_id: 'previous-assignee', to_user_id: toUserId }) };
    await mount(tasksAccess('read'), {
      projectStudioTasksListRequest: { tasks: [info], total: 1 }, projectStudioTaskGetRequest: { detail: detailFixture(info, { events: [event] }) },
      projectStudioNotificationsListRequest: { notifications: [notification], unreadCount: 0, hasMore: false },
    });
    click(document.querySelector('[data-bell]')); await settle();
    const item = document.querySelector('[data-notif="0"]');
    assert.equal(item.querySelector('.ps-notif-title').textContent, expectedTitle);
    assert.equal(item.querySelector('.ps-notif-body').textContent, 'WF-7 · <b>Persistent task</b>');
    assert.equal(item.querySelector('b'), null);
    assert.equal(item.textContent.includes('Unlocalized server'), false);
    click(item); await settle();
    assert.ok(document.querySelector(`[data-task-event="${eventId}"]`).closest('.tf-timeline-item').classList.contains('ps-task-target'));
    assert.equal(calls.find((call) => call.kind === 'projectStudioTaskGetRequest').payload.taskId, info.taskId);
  }
});

function treeProject(id, name, projectAccess, overrides = {}) {
  return projectFixture(projectAccess, { projectId: id, name, path: `/${id}`, taskTypeSourceProjectId: id, keyPrefix: id === 'p0-project' ? 'WF' : 'CH', ...overrides });
}

function treeFixtures(projects, overrides = {}) {
  return {
    projectStudioProjectTreeRequest: { projects, breadcrumbs: [], canCreate: false, canAdminister: false },
    projectStudioProjectGetRequest: ({ projectId }) => ({ project: projects.find((project) => project.projectId === projectId) }),
    projectStudioTaskTypesListRequest: ({ projectId }) => ({ types: taskTypes(), sourceProjectId: projects.find((project) => project.projectId === projectId).taskTypeSourceProjectId }),
    ...overrides,
  };
}

function selectValue(control, value) {
  assert.ok(control); control.value = value;
  control.dispatchEvent(new CustomEvent('change', { detail: { value }, bubbles: true }));
}

function checkValue(control, checked) {
  assert.ok(control); control.checked = checked;
  control.dispatchEvent(new CustomEvent('change', { detail: { checked }, bubbles: true }));
}

test('an accessible child-only card exposes ancestor names without parent actions or hidden metadata', async () => {
  const child = treeProject('child', 'Private accessible child', tasksAccess('read'), { parentId: 'name-only', path: '/name-only/child', depth: 2, isPrivate: true, ownOpenTasks: 4, myOpenTasks: 2, overdueTasks: 1 });
  await mount(child.access, treeFixtures([child], { projectStudioProjectTreeRequest: { projects: [child], breadcrumbs: [{ projectId: 'name-only', name: 'Ancestor label only', depth: 1 }], canCreate: false, canAdminister: false } }), {});
  const cards = document.querySelectorAll('.ps-card[data-project-id]');
  assert.equal(cards.length, 1); assert.match(cards[0].textContent, /Ancestor label only/);
  click(cards[0]); await settle();
  const crumbs = document.querySelector('#ps-crumbs');
  assert.ok([...crumbs.querySelectorAll('span.tf-breadcrumb-item')].some((item) => item.textContent === 'Ancestor label only'));
  assert.equal(crumbs.querySelector('a[href="#ancestor:name-only"]'), null);
  click(document.querySelector('[data-project-picker]'));
  const nodes = document.querySelector('[data-picker-tree] tf-tree').nodes;
  assert.equal(nodes[0].disabled, true); assert.equal(nodes[0].actions, undefined);
  assert.equal(calls.some((call) => call.payload?.projectId === 'name-only'), false);
  assert.equal(document.querySelector('[data-new-child]'), null);
});

test('project picker retains searched nodes through input blur and selects on the first click', async () => {
  const reader = tasksAccess('read');
  const parent = treeProject('p0-project', 'Parent project', reader);
  const child = treeProject('child', 'Inherited work', reader, { parentId: parent.projectId, path: '/p0-project/child', depth: 2, ownOpenTasks: 1 });
  await mount(reader, treeFixtures([parent, child]), { projectId: parent.projectId });
  click(document.querySelector('[data-project-picker]'));
  const search = document.querySelector('[data-tree-search] input');
  search.value = 'Inherited';
  search.dispatchEvent(new Event('input', { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 230));
  const tree = document.querySelector('[data-picker-tree] tf-tree');
  const target = tree.querySelector('[data-node-id="child"] > .tf-tree__row > .tf-tree__label');
  assert.ok(target);
  search.dispatchEvent(new Event('change', { bubbles: true }));
  assert.ok(document.querySelector('[data-picker-tree] tf-tree') === tree, 'native input change retains the existing result tree');
  assert.equal(target.isConnected, true);
  checkValue(document.querySelector('[data-tree-open]'), true);
  const filtered = document.querySelector('[data-picker-tree] tf-tree');
  assert.ok(filtered !== tree, 'the semantic checkbox change refreshes its filtered results');
  assert.equal(filtered.nodes[0].children[0].id, child.projectId);
  click(filtered.querySelector('[data-node-id="child"] > .tf-tree__row > .tf-tree__label'));
  await settle();
  await new Promise((resolve) => setTimeout(resolve, 300));
  assert.equal(!!document.querySelector('[data-picker-tree]'), false);
  assert.equal(document.querySelector('tf-detail-header').getAttribute('title'), child.name);
  assert.ok(calls.some((call) => call.kind === 'projectStudioProjectGetRequest' && call.payload.projectId === child.projectId));
});

test('child creation distinguishes an Empty own preset from live parent inheritance without a root creator grant', async () => {
  const admin = access({ projectAdmin: true });
  const parent = treeProject('p0-project', 'Grouping parent', admin, { template: 'empty', modules: [] });
  let created;
  const projects = [parent];
  await mount(admin, treeFixtures(projects, {
    projectStudioProjectCreateRequest: (input) => { created = treeProject('created-child', input.name, admin, { parentId: parent.projectId, path: '/p0-project/created-child', depth: 2, template: input.template, modules: input.modules, isPrivate: input.isPrivate, inheritModules: input.inheritModules, inheritTaskTypes: input.inheritTaskTypes }); projects.push(created); return { projectId: created.projectId }; },
  }), { projectId: parent.projectId });
  assert.equal(document.querySelector('#ps-new').hidden, true);
  click(document.querySelector('[data-new-child]'));
  document.querySelector('[data-child-name]').value = 'Explicit empty child';
  document.querySelector('[data-child-prefix]').value = 'EMPTY';
  selectValue(document.querySelector('[data-child-template]'), 'empty');
  assert.match(document.querySelector('[data-child-effective]').textContent, /Current parent modules/);
  checkValue(document.querySelector('[data-child-inherit-modules]'), false);
  checkValue(document.querySelector('[data-child-inherit-types]'), false);
  checkValue(document.querySelector('[data-child-private]'), true);
  assert.match(document.querySelector('[data-child-effective]').textContent, /Own preset modules/);
  assert.equal(document.querySelector('[data-child-effective] tf-chip'), null);
  click(document.querySelector('[data-child-name]').closest('tf-window').querySelector('[data-action="save"]')); await settle();
  const request = calls.find((call) => call.kind === 'projectStudioProjectCreateRequest');
  assert.deepEqual(request.payload, { name: 'Explicit empty child', description: '', keyPrefix: 'EMPTY', template: 'empty', modules: [], parentId: parent.projectId, isPrivate: true, inheritModules: false, inheritTaskTypes: false, members: [] });
  assert.equal(document.querySelector('tf-detail-header').getAttribute('title'), created.name);
});

test('a Settings reader sees current inheritance without sending a write-protected preview', async () => {
  const reader = access({ areas: [{ area: 'settings', enabled: true, level: 'read' }] });
  const child = treeProject('child', 'Read-only inheritance', reader, { parentId: 'parent', path: '/parent/child', depth: 2, inheritModules: true, inheritTaskTypes: true });
  await mount(reader, treeFixtures([child], { projectStudioSettingsGetRequest: { settings: { modules: child.modules, tags: [], agents: [] } }, agentsListRequest: { agentsJson: '[]' } }), { projectId: child.projectId, tab: 'settings' });
  assert.equal(document.querySelector('[data-inheritance-save]'), null);
  assert.ok(document.querySelector('[data-inheritance-modules]').hasAttribute('disabled'));
  assert.equal(calls.some((call) => call.kind === 'projectStudioProjectInheritancePreviewRequest'), false);
  assert.match(document.querySelector('[data-inheritance-diff]').textContent, /parent/i);
});

test('detaching and re-inheriting use the actual diff and one atomic inheritance save', async () => {
  const writer = access({ areas: [{ area: 'settings', enabled: true, level: 'write' }] });
  const child = treeProject('child', 'Live child', writer, { parentId: 'parent', path: '/parent/child', depth: 2, inheritModules: true, inheritTaskTypes: true });
  await mount(writer, treeFixtures([child], {
    projectStudioSettingsGetRequest: { settings: { modules: child.modules, tags: [], agents: [] } }, agentsListRequest: { agentsJson: '[]' },
    projectStudioProjectInheritancePreviewRequest: (input) => ({ currentModules: child.modules, proposedModules: input.inheritModules ? ['knowledge', 'tasks', 'tests'] : child.modules, currentTaskTypes: taskTypes(), proposedTaskTypes: taskTypes() }),
    projectStudioProjectInheritanceSaveRequest: (input) => { Object.assign(child, { inheritModules: input.inheritModules, inheritTaskTypes: input.inheritTaskTypes, isPrivate: input.isPrivate }); return { ok: true, effectiveModules: child.modules, taskTypeSourceProjectId: child.projectId }; },
  }), { projectId: child.projectId, tab: 'settings' });
  await settle();
  click(document.querySelector('[data-detach]')); await settle();
  assert.equal(document.querySelector('[data-inheritance-modules]').checked, false);
  assert.equal(document.querySelector('[data-inheritance-types]').checked, false);
  checkValue(document.querySelector('[data-inheritance-private]'), true); await settle();
  click(document.querySelector('[data-inheritance-save]')); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioProjectInheritanceSaveRequest').payload, { projectId: 'child', isPrivate: true, inheritModules: false, inheritTaskTypes: false, modules: child.modules });
  click(document.querySelector('[data-reinherit]')); await settle();
  const previews = calls.filter((call) => call.kind === 'projectStudioProjectInheritancePreviewRequest');
  assert.deepEqual(previews.at(-1).payload, { projectId: 'child', inheritModules: true, inheritTaskTypes: true });
  assert.match(document.querySelector('[data-inheritance-diff]').textContent, /Tests/);
  assert.equal(calls.filter((call) => call.kind === 'projectStudioProjectInheritanceSaveRequest').length, 1);
  click(document.querySelector('[data-inheritance-save]')); await settle();
  assert.deepEqual(calls.filter((call) => call.kind === 'projectStudioProjectInheritanceSaveRequest').at(-1).payload, { projectId: 'child', isPrivate: true, inheritModules: true, inheritTaskTypes: true, modules: ['knowledge', 'tasks', 'tests'] });
});

test('physical move permits root without a creator grant and excludes cycles, depth five and non-owned targets', async () => {
  const owner = access({ isOwner: true, areas: [{ area: 'settings', enabled: true, level: 'write' }] });
  const moving = treeProject('moving', 'Moving subtree', owner, { parentId: 'root', path: '/root/moving', depth: 2 });
  const leaf = treeProject('leaf', 'Existing grandchild', owner, { parentId: 'moving', path: '/root/moving/leaf', depth: 3 });
  const target = treeProject('target', 'Valid destination', owner);
  const deep = treeProject('deep', 'Too deep destination', owner, { depth: 3, path: '/a/b/deep', parentId: 'b' });
  const denied = treeProject('denied', 'Non-owner destination', access());
  await mount(owner, treeFixtures([moving, leaf, target, deep, denied], { projectStudioSettingsGetRequest: { settings: { modules: moving.modules, tags: [], agents: [] } }, agentsListRequest: { agentsJson: '[]' }, projectStudioProjectMoveRequest: () => { moving.parentId = null; moving.path = '/moving'; moving.depth = 1; return { ok: true }; } }), { projectId: moving.projectId, tab: 'settings' });
  document.querySelector('.ps-subproject-layout tf-tree').dispatchEvent(new CustomEvent('move', { detail: { id: moving.projectId, parentId: deep.projectId }, bubbles: true }));
  assert.equal(document.querySelector('[data-move-parent]'), null);
  assert.equal(calls.some((call) => call.kind === 'projectStudioProjectMoveRequest'), false);
  click(document.querySelector('[data-child-move]'));
  const selector = document.querySelector('[data-move-parent]');
  assert.deepEqual([...selector.querySelectorAll('option')].map((item) => item.value), ['', 'target']);
  click(selector.closest('tf-window').querySelector('[data-action="save"]')); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioProjectMoveRequest').payload, { projectId: 'moving', newParentId: null });
});

test('aggregate List and Board use authoritative counts, source filters and owning-project status rights', async () => {
  const root = treeProject('p0-project', 'Root aggregate', tasksAccess('write', 'write'), { descendantOpenTasks: 12 });
  const child = treeProject('child', 'Read-only child', tasksAccess('read', 'read'), { parentId: root.projectId, path: '/p0-project/child', depth: 2 });
  const rows = [taskFixture({ taskId: 'root-task', status: 'todo' }), taskFixture({ projectId: 'child', projectName: child.name, taskId: 'child-task', taskKey: 'CH-9', status: 'todo', taskType: 'custom_review', taskTypeName: 'Actual child review' })];
  const summary = { todo: 21, inProgress: 3, review: 4, done: 2, ownTotal: 10, descendantTotal: 20, sourceRevision: 103, appliedRevision: 100, indexedAt: '2026-10-01T12:00:00Z' };
  await mount(root.access, treeFixtures([root, child], { projectStudioTasksListRequest: { tasks: rows, total: 30, summary } }), { projectId: root.projectId, tab: 'tasks' });
  assert.equal(calls.find((call) => call.kind === 'projectStudioTasksListRequest').payload.scope, 'descendants');
  const table = document.querySelector('#ps-tasks-table');
  assert.equal(table.rows[1].project, child.name); assert.equal(table.rows[1].type.label, 'Actual child review');
  assert.match(document.querySelector('.ps-table-footer').textContent, /30.*10.*20/);
  assert.match(document.querySelector('#ps-task-index-state').textContent, /3/);
  selectValue(document.querySelector('#ps-tasks-project-filter'), ['child']); await settle();
  assert.deepEqual(calls.filter((call) => call.kind === 'projectStudioTasksListRequest').at(-1).payload.sourceProjectIds, ['child']);
  selectValue(document.querySelector('#ps-tasks-mode'), 'board'); await settle();
  const board = document.querySelector('tf-kanban');
  assert.ok(board); assert.equal(board.columns[0].count, 21); assert.equal(board.cards.find((card) => card.id === 'child-task').disabled, true);
  board.dispatchEvent(new CustomEvent('card-move', { detail: { cardId: 'child-task', to: 'review' }, bubbles: true })); await settle();
  assert.equal(calls.some((call) => call.kind === 'projectStudioTaskStatusSetRequest'), false);
  selectValue(document.querySelector('#ps-tasks-mode'), 'list'); await settle();
});

test('delayed task renders cannot cross project navigation or replace a newer task filter', async () => {
  const stages = ['projectStudioMembersListRequest', 'projectStudioProjectTreeRequest', 'projectStudioTaskTypesListRequest', 'projectStudioTasksListRequest'];
  for (const stage of stages) {
    const first = treeProject('p0-project', 'First project', tasksAccess('write', 'write'));
    const current = treeProject('current-project', 'Current project', tasksAccess('write', 'write'));
    let delayedKind = null;
    let complete;
    const replies = {
      projectStudioMembersListRequest: () => ({ members: [] }),
      projectStudioProjectTreeRequest: ({ projectId }) => ({ projects: projectId ? [projectId === first.projectId ? first : current] : [first, current], breadcrumbs: [], canCreate: false, canAdminister: false }),
      projectStudioTaskTypesListRequest: ({ projectId }) => ({ types: [...taskTypes(), { typeId: 'current_type', name: projectId === first.projectId ? 'First type' : 'Current type', active: true, builtIn: false, sortOrder: 100 }], sourceProjectId: projectId }),
      projectStudioTasksListRequest: ({ projectId, search }) => ({ tasks: [taskFixture({ projectId: projectId, taskId: `${projectId}-${search || 'initial'}`, title: `${projectId}: ${search || 'initial'}` })], total: 1 }),
    };
    const fixtures = Object.fromEntries(stages.map((kind) => [kind, async (payload) => {
      const result = replies[kind](payload);
      if (kind !== delayedKind) return result;
      delayedKind = null;
      return new Promise((resolve, reject) => { complete = { resolve: () => resolve(result), reject }; });
    }]));
    await mount(first.access, treeFixtures([first, current], fixtures), { projectId: first.projectId });
    delayedKind = stage;
    selectValue(document.querySelector('#ps-project-tabs'), 'tasks');
    for (let i = 0; i < 20 && !complete; i++) await flush();
    assert.ok(complete, `${stage} must actually remain pending`);
    const stale = complete; complete = null;
    click(document.querySelector('#ps-crumbs a.tf-breadcrumb-item'));
    await settle();
    click(document.querySelector(`.ps-card[data-project-id="${current.projectId}"]`)); await settle();
    selectValue(document.querySelector('#ps-project-tabs'), 'tasks'); await settle();
    const currentTable = document.querySelector('#ps-tasks-table');
    assert.equal(currentTable.rows[0].title, `${current.projectId}: initial`);
    stale.resolve(); await settle();
    assert.equal(document.querySelector('#ps-tasks-table'), currentTable, `${stage} cannot replace the current task table`);
    assert.equal(currentTable.rows[0].title, `${current.projectId}: initial`);
    assert.equal(document.querySelector('tf-detail-header').getAttribute('title'), current.name);
    assert.ok(document.querySelector('#ps-tasks-f-type').textContent.includes('Current type'));
    selectValue(document.querySelector('#ps-tasks-scope'), 'single'); await settle();
    assert.equal(calls.filter((call) => call.kind === 'projectStudioTasksListRequest').at(-1).payload.projectId, current.projectId);

    if (stage === 'projectStudioTasksListRequest') {
      delayedKind = stage;
      document.querySelector('#ps-tasks-search').dispatchEvent(new CustomEvent('search', { detail: { value: 'earlier' }, bubbles: true }));
      await settle(); assert.ok(complete); const earlier = complete; complete = null;
      document.querySelector('#ps-tasks-search').dispatchEvent(new CustomEvent('search', { detail: { value: 'latest' }, bubbles: true }));
      await settle();
      const latestTable = document.querySelector('#ps-tasks-table');
      assert.equal(latestTable.rows[0].title, `${current.projectId}: latest`);
      earlier.resolve(); await settle();
      assert.equal(document.querySelector('#ps-tasks-table'), latestTable);
      assert.equal(latestTable.rows[0].title, `${current.projectId}: latest`);
      delayedKind = stage;
      document.querySelector('#ps-tasks-search').dispatchEvent(new CustomEvent('search', { detail: { value: 'leave' }, bubbles: true }));
      await settle(); assert.ok(complete); const failed = complete; complete = null;
      selectValue(document.querySelector('#ps-project-tabs'), 'overview'); await settle();
      const overview = document.querySelector('#ps-tab-panel').innerHTML;
      failed.reject(new Error('Delayed previous task view failed')); await settle();
      assert.equal(document.querySelector('#ps-tab-panel').innerHTML, overview);
      assert.equal(document.querySelector('#ps-tasks-scope'), null);
    }
  }
});

test('an aggregate without a scope timestamp reports unavailable timing and preserves actual lag', async () => {
  const root = treeProject('p0-project', 'Root aggregate', tasksAccess('write', 'write'));
  const child = treeProject('child', 'Current child', tasksAccess('read', 'read'), { parentId: root.projectId, path: '/p0-project/child', depth: 2 });
  const rows = Array.from({ length: 7 }, (_, index) => taskFixture({ projectId: 'child', taskId: `child-${index}`, taskKey: `CH-${index + 1}`, status: 'todo' }));
  const summary = { todo: 7, inProgress: 0, review: 0, done: 0, ownTotal: 0, descendantTotal: 7, sourceRevision: 7, appliedRevision: 7, indexedAt: null };
  await mount(root.access, treeFixtures([root, child], { projectStudioTasksListRequest: { tasks: rows, total: 7, summary } }), { projectId: root.projectId, tab: 'tasks' });
  const timing = () => document.querySelector('#ps-task-index-state');
  assert.equal(document.querySelector('#ps-tasks-table').rows.length, 7);
  assert.equal(timing().textContent, I18n.t('project_studio.index_time_unavailable'));
  assert.equal(timing().querySelector('tf-chip'), null);
  summary.sourceRevision = 9;
  selectValue(document.querySelector('#ps-tasks-mode'), 'board'); await settle();
  assert.ok(timing().textContent.includes(I18n.t('project_studio.index_time_unavailable')));
  assert.equal(timing().querySelector('tf-chip').textContent, I18n.t('project_studio.index_lag', { count: 2 }));
  for (const language of ['pl', 'en', 'de', 'es', 'fr']) {
    const dictionary = JSON.parse(readFileSync(new URL(`../../i18n/${language}.json`, import.meta.url))).project_studio;
    assert.equal(typeof dictionary.index_time_unavailable, 'string');
    assert.equal('index_waiting' in dictionary, false);
  }
  selectValue(document.querySelector('#ps-tasks-mode'), 'list'); await settle();
});

test('inherited writers remain eligible after a local grant expires, with local-only administration', async () => {
  const admin = tasksAccess(); admin.projectAdmin = true; admin.canManageMembers = true;
  const info = taskFixture();
  const origins = [{ projectId: 'ancestor', name: 'Named ancestor', depth: 1 }];
  const members = [
    { userId: 'inherited', displayName: 'Inherited writer', inherited: true, originProjects: origins, active: true, functions: [], projectAdmin: false, expiresAt: null, access: tasksAccess() },
    { userId: 'expired-local', displayName: 'Expired local with active ancestor', inherited: false, originProjects: origins, active: false, functions: ['observer'], projectAdmin: false, expiresAt: '2020-01-01T00:00:00Z', access: tasksAccess() },
    { userId: 'expired-all', displayName: 'Expired everywhere', inherited: false, originProjects: [], active: false, functions: ['developer'], expiresAt: '2020-01-01T00:00:00Z', access: access({ hasAccess: false, canCreateTasks: false }) },
  ];
  await mount(admin, { projectStudioMembersListRequest: { members }, projectStudioTasksListRequest: { tasks: [info], total: 1 }, projectStudioTaskGetRequest: { detail: detailFixture(info) }, projectStudioMembersAddRequest: { added: 1 } });
  await openTaskCard();
  assert.deepEqual([...document.querySelectorAll('#ps-task-assignee option')].map((option) => option.value), ['', 'inherited', 'expired-local']);
  assert.deepEqual(document.querySelector('#ps-task-mentions').items.map((item) => item.id), ['inherited', 'expired-local']);
  click(document.querySelector('#ps-task-title').closest('tf-window').querySelector('[data-action="cancel"]')); await settle();
  click(document.querySelector('[data-goto-members]')); await settle();
  const table = document.querySelector('[data-member-table="inherited"]');
  assert.match(table.rows[0].person, /Named ancestor/);
  const menu = table.rowActions(table.rows[0]); document.body.appendChild(menu); click(menu);
  const labels = [...document.querySelectorAll('tf-menu-item')].map((item) => item.getAttribute('label'));
  assert.ok(labels.includes('Add a local grant')); assert.equal(labels.includes('Remove'), false);
  menuAction('Add a local grant');
  assert.equal(document.querySelectorAll('[data-member-function][checked]').length, 0);
  checkValue(document.querySelector('[data-member-function="tester"]'), true);
  click(document.querySelector('[data-member-expiry]').closest('tf-window').querySelector('[data-action="save"]')); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioMembersAddRequest').payload, { projectId: 'p0-project', members: [{ userId: 'inherited', functions: ['tester'], projectAdmin: false, expiresAt: null }] });
  assert.equal(calls.some((call) => call.kind === 'projectStudioMemberAccessSetRequest'), false);
  assert.match(document.querySelector('[data-member-table="expired"]').rows.find((row) => row._id === 'expired-local').person, /Named ancestor/);
  const expiredTable = document.querySelector('[data-member-table="expired"]');
  const localMenu = expiredTable.rowActions(expiredTable.rows.find((row) => row._id === 'expired-local'));
  document.body.appendChild(localMenu); click(localMenu); menuAction(I18n.t('project_studio.members_handover'));
  assert.deepEqual(navigation.at(-1), { view: 'org-structure', params: { tab: 'list', handover: 'expired-local', reason: 'project_removal', project: 'p0-project' } });
});

test('lifecycle resolves paged open tasks with a real reason before end becomes admissible', async () => {
  const owner = tasksAccess(); owner.isOwner = true;
  const project = treeProject('p0-project', 'Lifecycle project', owner);
  let open = 63;
  await mount(owner, treeFixtures([project], {
    projectStudioProjectLifecyclePreviewRequest: () => ({ openTaskCount: open, activeChildren: [], exclusiveMemberCount: 2, canEnd: open === 0 }),
    projectStudioTasksListRequest: (input) => ({ tasks: [taskFixture({ taskId: `open-${input.offset}`, taskKey: `WF-${input.offset + 1}`, status: input.status })], total: input.status === 'todo' ? 63 : 1 }),
    projectStudioTaskNotPursuedRequest: () => { open = 0; return { ok: true, eventId: 91 }; },
    projectStudioProjectLifecycleRequest: (input) => { project.lifecycle = 'ended'; project.access = { ...owner, ended: true }; return { ok: true }; },
  }), { projectId: project.projectId });
  click(document.querySelector('[data-lifecycle]')); await settle();
  assert.ok(document.querySelector('[data-action="apply"]').hasAttribute('disabled'));
  let table = document.querySelector('[data-lifecycle-tasks] tf-table');
  table.dispatchEvent(new CustomEvent('page-change', { detail: { page: 3 }, bubbles: true })); await settle();
  assert.equal(calls.filter((call) => call.kind === 'projectStudioTasksListRequest').at(-1).payload.offset, 50);
  selectValue(document.querySelector('[data-lifecycle-status]'), 'review'); await settle();
  const request = calls.filter((call) => call.kind === 'projectStudioTasksListRequest').at(-1).payload;
  assert.equal(request.status, 'review'); assert.equal(request.offset, 0); assert.equal(request.scope, 'single');
  table = document.querySelector('[data-lifecycle-tasks] tf-table');
  const action = table.rowActions(table.rows[0]); document.body.appendChild(action); click(action); menuAction('Will not be pursued');
  const resolution = document.querySelector('[data-resolution-reason]');
  click(resolution.closest('tf-window').querySelector('[data-action="save"]')); await settle();
  assert.equal(calls.some((call) => call.kind === 'projectStudioTaskNotPursuedRequest'), false);
  resolution.value = 'The contract was completed elsewhere.';
  click(resolution.closest('tf-window').querySelector('[data-action="save"]')); await settle();
  assert.equal(calls.find((call) => call.kind === 'projectStudioTaskNotPursuedRequest').payload.reason, resolution.value);
  assert.equal(document.querySelector('[data-action="apply"]').hasAttribute('disabled'), false);
  document.querySelector('[data-lifecycle-confirm]').value = project.keyPrefix;
  document.querySelector('[data-lifecycle-reason]').value = 'All remaining work was resolved.';
  click(document.querySelector('[data-action="apply"]')); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioProjectLifecycleRequest').payload, { projectId: project.projectId, ended: true, confirmationKey: 'WF', reason: 'All remaining work was resolved.' });
  assert.equal(document.querySelector('[data-new-task]'), null);
});

test('transfer checks the actual closure and wider access confirmation before opening the canonical destination', async () => {
  const owner = tasksAccess(); owner.isOwner = true;
  const root = treeProject('p0-project', 'Source project', owner);
  const destination = treeProject('destination', 'Destination project', owner);
  let info = taskFixture();
  let blocked = true;
  await mount(owner, treeFixtures([root, destination], {
    projectStudioTasksListRequest: () => ({ tasks: [info], total: 1 }),
    projectStudioTaskGetRequest: () => ({ detail: detailFixture(info) }),
    projectStudioTaskTransferPreviewRequest: () => ({ taskIds: [info.taskId, 'subtask'], oldKeys: ['WF-7', 'WF-8'], destinationTypeValid: true, widensAccess: true, blockingReasons: blocked ? ['attachment_unavailable'] : [] }),
    projectStudioTaskTransferRequest: () => { info = { ...info, projectId: destination.projectId, projectName: destination.name, taskKey: 'CH-12' }; return { operationId: 'transfer-op', taskId: info.taskId, destinationProjectId: destination.projectId, newKey: info.taskKey, movedTaskIds: [info.taskId, 'subtask'] }; },
    projectStudioTaskKeyResolveRequest: (input) => ({ projectId: info.projectId, taskId: info.taskId, currentKey: info.taskKey, eventId: null }),
  }), { projectId: root.projectId, tab: 'tasks' });
  const table = document.querySelector('#ps-tasks-table'); table.dispatchEvent(new CustomEvent('row-click', { detail: { row: table.rows[0] }, bubbles: true })); await settle();
  click(document.querySelector('[data-action="transfer"]')); await settle();
  selectValue(document.querySelector('[data-transfer-destination]'), destination.projectId); await settle();
  assert.match(document.querySelector('[data-transfer-preview]').textContent, /WF-7.*WF-8/);
  assert.match(document.querySelector('[data-transfer-preview]').textContent, /original attachment is unavailable/i);
  assert.ok(document.querySelector('[data-transfer-preview]').closest('tf-window').querySelector('[data-action="transfer"]').hasAttribute('disabled'));
  blocked = false; selectValue(document.querySelector('[data-transfer-destination]'), destination.projectId); await settle();
  const preview = document.querySelector('[data-transfer-preview]');
  const submit = preview.closest('tf-window').querySelector('[data-action="transfer"]');
  assert.ok(submit.hasAttribute('disabled'));
  checkValue(preview.querySelector('[data-confirm-wider]'), true);
  click(submit); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioTaskTransferRequest').payload, { sourceProjectId: root.projectId, destinationProjectId: destination.projectId, taskId: info.taskId, confirmWiderAccess: true });
  assert.equal(document.querySelector('tf-detail-header').getAttribute('title'), destination.name);
  const card = [...document.querySelectorAll('#ps-task-title')].at(-1).closest('tf-window');
  assert.match(card.shadowRoot.querySelector('.tf-window-title-text').textContent, /CH-12/);
});

test('an old task key and origin event resolve before fetching source metadata or opening current history', async () => {
  const reader = tasksAccess('read');
  const destination = treeProject('destination', 'Current permitted project', reader);
  const info = taskFixture({ projectId: destination.projectId, projectName: destination.name, taskKey: 'CH-99' });
  await mount(reader, treeFixtures([destination], {
    projectStudioTaskKeyResolveRequest: (input) => { assert.deepEqual(input, { taskKey: 'OLD-7', taskId: null, originProjectId: 'now-private-source', originEventId: '777' }); return { projectId: destination.projectId, taskId: info.taskId, currentKey: info.taskKey, eventId: 33 }; },
    projectStudioTasksListRequest: { tasks: [info], total: 1 },
    projectStudioTaskGetRequest: { detail: detailFixture(info, { events: [{ eventId: 33, taskId: info.taskId, kind: 'transferred', actorKind: 'user', actorId: '', at: '2026-10-01T12:00:00Z', beforeJson: 'null', afterJson: '{"task_key":"CH-99","project_id":"destination","project_name":"Current permitted project"}' }] }) },
  }), { projectId: 'now-private-source', taskKey: 'OLD-7', eventId: '777' });
  assert.equal(calls.some((call) => call.kind === 'projectStudioProjectGetRequest' && call.payload.projectId === 'now-private-source'), false);
  assert.match(document.querySelector('#ps-task-history').textContent, /CH-99[\s\S]*Current permitted project/);
  assert.equal(document.querySelector('#ps-task-history').textContent.includes('now-private-source'), false);
  assert.ok(document.querySelector('#ps-task-history [data-event-id="33"]') || document.querySelector('#ps-task-history').textContent.includes('Transferred'));
});

test('an own Empty node saves the exact empty module set and inherited modules remain read-only', async () => {
  const writer = access({ areas: [{ area: 'settings', enabled: true, level: 'write' }] });
  const node = treeProject('empty-node', 'Empty grouping node', writer, { template: 'empty', modules: [] });
  await mount(writer, treeFixtures([node], { projectStudioSettingsGetRequest: { settings: { modules: [], tags: [], agents: [] } }, agentsListRequest: { agentsJson: '[]' }, projectStudioSettingsSaveRequest: { ok: true } }), { projectId: node.projectId, tab: 'settings' });
  assert.equal(document.querySelector('#ps-set-modules tf-toggle[checked]'), null);
  assert.equal(document.querySelector('#ps-set-modules tf-toggle[data-module="knowledge"]').hasAttribute('disabled'), false);
  click(document.querySelector('#ps-set-modules-save')); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioSettingsSaveRequest').payload, { projectId: node.projectId, modules: [] });
  node.parentId = 'parent'; node.path = '/parent/empty-node'; node.depth = 2; node.inheritModules = true;
  await mount(writer, treeFixtures([node], { projectStudioSettingsGetRequest: { settings: { modules: [], tags: [], agents: [] } }, agentsListRequest: { agentsJson: '[]' } }), { projectId: node.projectId, tab: 'settings' });
  assert.ok(document.querySelector('#ps-set-modules tf-toggle[data-module="knowledge"]').hasAttribute('disabled'));
  assert.ok(document.querySelector('#ps-set-modules-save').hasAttribute('disabled'));
  click(document.querySelector('#ps-set-modules-save')); await settle();
  assert.equal(calls.some((call) => call.kind === 'projectStudioSettingsSaveRequest'), false);
});

test('new-task project selection reloads the destination catalogue and preserves the actual draft', async () => {
  const root = treeProject('p0-project', 'Draft source', tasksAccess());
  const child = treeProject('child', 'Draft destination', tasksAccess(), { parentId: root.projectId, path: '/p0-project/child', depth: 2 });
  await mount(root.access, treeFixtures([root, child], { projectStudioTaskSaveRequest: { taskId: 'created', taskNo: 2, taskKey: 'CH-2', eventIds: [1] } }), { projectId: root.projectId });
  click(document.querySelector('[data-new-task]')); await settle();
  document.querySelector('#ps-task-title').value = 'Retained draft title'; document.querySelector('#ps-task-title').dispatchEvent(new Event('input', { bubbles: true }));
  selectValue(document.querySelector('#ps-task-project'), child.projectId); await settle();
  const form = [...document.querySelectorAll('#ps-task-title')].at(-1).closest('tf-window');
  assert.equal(form.querySelector('#ps-task-title').value, 'Retained draft title');
  assert.equal(document.querySelector('tf-detail-header').getAttribute('title'), child.name);
  assert.equal(calls.filter((call) => call.kind === 'projectStudioTaskTypesListRequest').at(-1).payload.projectId, child.projectId);
  click(form.querySelector('[data-action="save"]')); await settle();
  assert.equal(calls.find((call) => call.kind === 'projectStudioTaskSaveRequest').payload.projectId, child.projectId);
});

test('archive and export preview the exact selected scope before submitting the same scope', async () => {
  const owner = access({ isOwner: true, projectAdmin: true });
  const root = treeProject('p0-project', 'Scoped root', owner);
  const child = treeProject('child', 'Scoped child', owner, { parentId: root.projectId, path: '/p0-project/child', depth: 2 });
  await mount(owner, treeFixtures([root, child], {
    projectStudioProjectScopePreviewRequest: ({ scope }) => ({ nodes: (scope === 'subtree' ? [root, child] : [root]).map(({ projectId, name, depth }) => ({ projectId, name, depth })) }),
    projectStudioProjectArchiveRequest: { ok: true }, projectStudioProjectExportStartRequest: { jobId: 'export' },
    projectStudioProjectExportStatusRequest: { status: 'success', progressPct: 100, phase: 'done', signedUrl: 'https://example.invalid/authorized-download', archiveBytes: 512000, inventory: { tasks: 2 } },
  }), {});
  const card = document.querySelector('[data-project-id="p0-project"]');
  click(card.querySelector('[data-more]'));
  click(card.querySelector('tf-menu-item[action="archive"] .tf-menu-item')); await settle();
  selectValue(document.querySelector('[data-archive-scope]'), 'subtree'); await settle();
  assert.match(document.querySelector('[data-scope-nodes]').textContent, /Scoped root.*Scoped child/);
  click(document.querySelector('[data-archive-scope]').closest('tf-window').querySelector('[data-action="archive"]')); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioProjectArchiveRequest').payload, { projectId: root.projectId, archived: true, scope: 'subtree' });
  click(document.querySelector('[data-project-id="p0-project"]')); await settle();
  click(document.querySelector('[data-export]')); await settle();
  selectValue(document.querySelector('#ps-export-scope'), 'subtree'); await settle();
  assert.match(document.querySelector('[data-export-nodes]').textContent, /Scoped root.*Scoped child/);
  click(document.querySelector('#ps-export-scope').closest('tf-window').querySelector('[data-action="start"]')); await settle();
  assert.equal(calls.find((call) => call.kind === 'projectStudioProjectExportStartRequest').payload.scope, 'subtree');
  const operations = calls.filter((call) => call.kind === 'projectStudioProjectScopePreviewRequest' && call.payload.scope === 'subtree').map((call) => call.payload.operation);
  assert.deepEqual(operations, ['archive', 'export']);
});

test('unarchive previews the selected owned subtree and preserves errors without a premature mutation', async () => {
  const owner = access({ isOwner: true, projectAdmin: true, archived: true });
  const root = treeProject('p0-project', 'Archived root', owner);
  const child = treeProject('child', 'Archived child', owner, { parentId: root.projectId, path: '/p0-project/child', depth: 2 });
  let completePreview;
  let mutationDenied = true;
  await mount(owner, treeFixtures([root, child], {
    projectStudioProjectScopePreviewRequest: ({ scope }) => {
      if (scope !== 'subtree') throw new Error('project has children; select subtree scope');
      return new Promise((resolve) => { completePreview = () => resolve({ nodes: [root, child].map(({ projectId, name, depth }) => ({ projectId, name, depth })) }); });
    },
    projectStudioProjectArchiveRequest: () => {
      if (mutationDenied) throw new Error('Current owner access changed');
      for (const project of [root, child]) Object.assign(project, { status: 'active', access: { ...owner, archived: false }, canArchive: true, canUnarchive: false });
      return { ok: true };
    },
  }), {});
  document.querySelector('#ps-filter').dispatchEvent(new CustomEvent('change', { detail: { id: 'all' }, bubbles: true })); await settle();
  const card = document.querySelector('[data-project-id="p0-project"]');
  click(card.querySelector('[data-more]'));
  click(card.querySelector('tf-menu-item[action="unarchive"] .tf-menu-item')); await settle();
  const window = document.querySelector('[data-archive-scope]').closest('tf-window');
  selectValue(window.querySelector('[data-archive-scope]'), 'node'); await settle();
  const submit = window.querySelector('[data-action="archive"]');
  assert.equal(submit.textContent, I18n.t('project_studio.action_unarchive'));
  assert.match(window.querySelector('[data-form-error]').textContent, /select subtree scope/);
  assert.ok(submit.hasAttribute('disabled'));
  assert.equal(calls.some((call) => call.kind === 'projectStudioProjectArchiveRequest'), false);
  selectValue(window.querySelector('[data-archive-scope]'), 'subtree'); await settle();
  assert.ok(submit.hasAttribute('disabled'));
  assert.equal(window.querySelector('[data-form-error]').hidden, true);
  click(submit); await settle();
  assert.equal(calls.some((call) => call.kind === 'projectStudioProjectArchiveRequest'), false);
  completePreview(); await settle();
  assert.match(window.querySelector('[data-scope-nodes]').textContent, /Archived root.*Archived child/);
  click(submit); await settle();
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioProjectArchiveRequest').payload, { projectId: root.projectId, archived: false, scope: 'subtree' });
  assert.match(window.querySelector('[data-form-error]').textContent, /Current owner access changed/);
  assert.equal(root.status, 'archived');
  mutationDenied = false; click(submit); await settle();
  assert.equal(root.status, 'active'); assert.equal(child.status, 'active');
  for (const language of ['pl', 'en', 'de', 'es', 'fr']) {
    const dictionary = JSON.parse(readFileSync(new URL(`../../i18n/${language}.json`, import.meta.url))).project_studio;
    for (const key of ['action_unarchive', 'archive_scope', 'scope_node', 'scope_subtree', 'action_cancel']) assert.equal(typeof dictionary[key], 'string');
  }
});

test('unarchive does not expose a mutation or scope preview without current ownership', async () => {
  const reader = access({ archived: true });
  const project = treeProject('p0-project', 'Archived read-only project', reader);
  await mount(reader, treeFixtures([project]), {});
  document.querySelector('#ps-filter').dispatchEvent(new CustomEvent('change', { detail: { id: 'all' }, bubbles: true })); await settle();
  const card = document.querySelector('[data-project-id="p0-project"]');
  assert.equal(card.querySelector('tf-menu-item[action="unarchive"]'), null);
  card.querySelector('tf-menu').dispatchEvent(new CustomEvent('action', { detail: { action: 'unarchive' }, bubbles: true })); await settle();
  assert.equal(document.querySelector('[data-archive-scope]'), null);
  assert.equal(calls.some((call) => ['projectStudioProjectScopePreviewRequest', 'projectStudioProjectArchiveRequest'].includes(call.kind)), false);
});

test('archive import uploads bounded slices and distinguishes source prefixes from the actual saved tree', async () => {
  const project = projectFixture(access());
  const nodes = ['One', 'Two', 'Three', 'Four'].map((name, index) => ({ projectId: `node-${index}`, parentId: index ? `node-${index - 1}` : null, name, keyPrefix: `N${index}`, depth: index + 1, isPrivate: index === 3, lifecycle: index === 3 ? 'ended' : 'active', inventory: { tasks: index + 1, files: index, bytesFiles: index * 512 } }));
  const saved = nodes.map((node, index) => treeProject(`clone-${index}`, `${node.name} copy`, project.access, { parentId: index ? `clone-${index - 1}` : null, path: '/' + nodes.slice(0, index + 1).map((entry, position) => `clone-${position}`).join('/'), depth: index + 1, keyPrefix: `COPY${index}` }));
  const slices = [];
  const file = { name: 'Actual staged tree.tfproj.zip', size: 1024 * 1024 + 7, arrayBuffer() { throw new Error('No full archive read'); }, slice(start, end) { slices.push([start, end]); return { arrayBuffer: async () => new Uint8Array(end - start).buffer }; } };
  await mount(project.access, treeFixtures([project], {
    projectStudioProjectTreeRequest: ({ projectId }) => ({ projects: projectId === saved[0].projectId ? saved : [project], breadcrumbs: [], canCreate: true, canAdminister: false }),
    projectStudioProjectImportUploadChunkRequest: { ok: true },
    projectStudioProjectImportPreviewRequest: { projectName: 'One', modules: ['tasks'], template: 'deployment', archiveVersion: 2, totalUncompressedBytes: file.size, exportedAt: '2026-10-01T12:00:00Z', vectorsReusable: false, vectorsReason: 'No vectors', hasRuns: false, inventory: { tasks: 10 }, treeNodes: nodes },
    projectStudioProjectImportApplyRequest: { jobId: 'saved-import' },
    projectStudioProjectImportStatusRequest: { status: 'success', progressPct: 100, phase: 'done', projectId: saved[0].projectId, reindexJobIds: [], vectorsImported: false },
  }), {});
  click(document.querySelector('#ps-import')); await settle();
  document.querySelector('#ps-imp-file').dispatchEvent(new CustomEvent('change', { detail: { files: [file] }, bubbles: true }));
  click(document.querySelector('#ps-imp-file').closest('tf-window').querySelector('[data-action="upload"]')); await settle();
  assert.deepEqual(slices, [[0, 524288], [524288, 1048576], [1048576, 1048583]]);
  const chunks = calls.filter((call) => call.kind === 'projectStudioProjectImportUploadChunkRequest');
  assert.deepEqual(chunks.map((call) => [call.payload.seq, call.payload.totalChunks, call.payload.bytes.length]), [[0, 3, 524288], [1, 3, 524288], [2, 3, 7]]);
  const table = document.querySelector('[data-import-tree]');
  assert.equal(table.querySelector('tf-column[key="prefix"]').getAttribute('label'), 'Source prefix');
  assert.match(table.parentElement.textContent, /collisions receive new unique values/);
  assert.equal(table.rows[3].name, 'One / Two / Three / Four'); assert.equal(table.rows[3].tasks, 4);
  assert.equal(table.rows[3].status, 'Ended'); assert.match(table.rows[3].visibility, /Private/);
  table.dispatchEvent(new CustomEvent('row-click', { detail: { row: table.rows[3] }, bubbles: true }));
  assert.match(table.parentElement.textContent, /Four/);
  assert.equal(calls.some((call) => call.kind === 'projectStudioProjectImportApplyRequest'), false);
  click(table.closest('tf-window').querySelector('[data-action="apply"]')); await settle();
  const actual = document.querySelector('[data-imported-tree]');
  assert.equal(actual.rows[3].name, 'One copy / Two copy / Three copy / Four copy');
  assert.deepEqual(actual.rows.map((row) => row.prefix), ['COPY0', 'COPY1', 'COPY2', 'COPY3']);
  assert.deepEqual(calls.find((call) => call.kind === 'projectStudioProjectTreeRequest' && call.payload.projectId === saved[0].projectId).payload, { projectId: saved[0].projectId, includeEnded: true, includeArchived: true });
  click(actual.closest('tf-window').querySelector('[data-action="close-import"]')); await settle();
  await new Promise((resolve) => setTimeout(resolve, 280));
  assert.equal(actual.isConnected, false);
});
