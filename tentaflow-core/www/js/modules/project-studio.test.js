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
await I18n.setLanguage('en');
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
  return { has_access: true, project_admin: false, app_admin: false, is_owner: false,
    archived: false, functions: [], expires_at: null, enabled_modules: ['knowledge', 'tasks'],
    areas: PROJECT_AREAS.map((area) => ({ area, enabled: ['knowledge', 'repos', 'tasks', 'board', 'sprints', 'settings'].includes(area), level: 'none' })),
    can_create_tasks: true, can_manage_members: false, can_manage_settings: false, ...overrides };
}

function catalogue() {
  return BUILTIN_FUNCTIONS.map((definition) => ({ ...definition,
    function_id: definition.functionId,
    name: definition.functionId === 'developer' ? 'Engineering' : definition.name,
    grants: PROJECT_AREAS.map((area) => ({ area, level: area === 'knowledge' ? 'read' : 'none' })),
  }));
}

async function mount(projectAccess, overrides = {}) {
  screen.unmount();
  calls.length = 0;
  navigation.length = 0;
  const project = { project_id: 'p0-project', name: 'Workflow', description: '', template: 'custom', status: projectAccess.archived ? 'archived' : 'active', modules: ['knowledge', 'tasks'], access: projectAccess, member_count: 3 };
  const fixtures = {
    // An AuthMe role never substitutes for project or application permissions.
    authMeRequest: { userId: new Uint8Array(16), username: 'Creator', role: 'admin' },
    projectStudioProjectsListRequest: { projects: [project], can_create: false, can_administer: false },
    projectStudioCatalogueGetRequest: { functions: catalogue() },
    projectStudioProjectGetRequest: { project },
    projectStudioOverviewRequest: { kpis: {}, activity: [] },
    projectStudioNotificationsListRequest: { notifications: [], unread_count: 0 },
    projectStudioMembersListRequest: { members: [] },
    projectStudioTaskSaveRequest: { task_id: 'created-task', task_no: 1 },
    ...overrides,
  };
  ApiBinary.one = async (kind, payload) => {
    calls.push({ kind, payload });
    if (!(kind in fixtures)) throw new Error(`unexpected request ${kind}`);
    return typeof fixtures[kind] === 'function' ? fixtures[kind](payload) : fixtures[kind];
  };
  document.body.innerHTML = `<main>${screen.render()}</main>`;
  await screen.mount();
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
  assert.equal(document.querySelector('#ps-task-att-input'), null, 'no inaccessible attachment API is exposed');
  document.querySelector('tf-window [data-action="save"]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 4; i += 1) await flush();
  assert.equal(calls.filter((call) => call.kind === 'projectStudioTaskSaveRequest').length, 1);
  assert.equal(calls.some((call) => ['projectStudioTaskGetRequest', 'projectStudioTasksListRequest', 'projectStudioCasesListRequest', 'projectStudioSourcesListRequest'].includes(call.kind)), false);
  await new Promise((resolve) => setTimeout(resolve, 280));
  assert.ok(!document.querySelector('.ps-window-body #ps-task-title'), 'successful creation closes the form');
  screen.unmount();
});

test('members are grouped by actual active status and removal enters shared handover', async () => {
  const adminAccess = access({ project_admin: true, can_manage_members: true });
  const members = [
    { user_id: 'permanent', display_name: 'Permanent', active: true, functions: ['developer'], project_admin: false, expires_at: null, is_owner: false, access: access() },
    { user_id: 'temporary', display_name: 'Temporary', active: true, functions: ['tester', 'developer'], project_admin: false, expires_at: '2030-01-01T00:00:00Z', is_owner: false, access: access() },
    { user_id: 'expired', display_name: 'Expired', active: false, functions: ['tester'], project_admin: false, expires_at: '2020-01-01T00:00:00Z', is_owner: false, access: access({ has_access: false, can_create_tasks: false }) },
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
  const member = { user_id: 'temporary', display_name: 'Temporary', active: true, functions: ['tester'], project_admin: false, expires_at: null, is_owner: false, access: access() };
  await mount(access({ project_admin: true, can_manage_members: true }), {
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
  const member = { user_id: 'successor', display_name: 'Successor', active: true, functions: ['tester'], project_admin: false, expires_at: null, is_owner: false, access: access({ archived: true, can_create_tasks: false }) };
  await mount(access({ is_owner: true, project_admin: true, archived: true, can_create_tasks: false }), {
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
    projectStudioChatsListRequest: { chats: [{ chat_id: 'saved-chat', title: 'Saved conversation' }] },
    projectStudioChatHistoryRequest: { messages: [{ role: 'assistant', content: 'Saved answer', created_at: '2026-09-30T12:00:00Z', citations_json: '[]' }] },
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
  const info = { case_id: 'code-case', title: 'Code case', kind: 'api', status: 'draft', priority: 'medium', current_version: 1, language: 'python', tag_ids: [], linked_source_ids: [] };
  await mount(testAccess, {
    projectStudioCasesListRequest: { cases: [info], total: 1 },
    projectStudioCaseGetRequest: { detail: { info, content_json: JSON.stringify({ script: 'assert True', language: 'python', config: {} }), attachments: [], versions: [] } },
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
  const manual = { case_id: 'manual-case', title: 'Manual case', kind: 'manual', status: 'approved', priority: 'medium' };
  await mount(testAccess(), {
    projectStudioSettingsGetRequest: { settings: { tags: [] } },
    projectStudioCasesListRequest: { cases: [manual], total: 1 },
    projectStudioRunsListRequest: { runs: [], total: 0 },
    projectStudioSuitesListRequest: { suites: [{ suite_id: 'manual-suite', name: 'Manual suite', case_count: 1 }] },
    projectStudioSuiteGetRequest: { cases: [manual] },
    projectStudioRunnersListRequest: { runners: [] },
    projectStudioRunCreateRequest: { run_id: 'manual-run', run_no: 1 },
    projectStudioRunGetRequest: { run: { run_id: 'manual-run', run_no: 1, run_type: 'manual', name: 'Manual accessible run', status: 'running', total: 1, pending: 1 }, items: [] },
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
    { schedule_id: 'auto', name: 'Automatic schedule', run_type: 'auto', enabled: false, schedule_kind: 'interval', schedule_expr: '10m', timezone: 'UTC', environment_status: 'approved' },
    { schedule_id: 'perf', name: 'Performance schedule', run_type: 'perf', enabled: true, schedule_kind: 'interval', schedule_expr: '10m', timezone: 'UTC', environment_status: 'approved' },
    { schedule_id: 'manual', name: 'Manual schedule', run_type: 'manual', enabled: true, schedule_kind: 'interval', schedule_expr: '10m', timezone: 'UTC' },
  ];
  await mount(testAccess('admin'), {
    projectStudioSettingsGetRequest: { settings: { tags: [] } },
    projectStudioCasesListRequest: { cases: [], total: 0 },
    projectStudioSchedulesListRequest: { schedules, server_timezone: 'UTC' },
    projectStudioSuitesListRequest: { suites: [{ suite_id: 'manual-suite', name: 'Manual suite', case_count: 1 }] },
    projectStudioRunnersListRequest: { runners: [] },
    projectStudioScheduleSetEnabledRequest: { ok: true },
    projectStudioScheduleSaveRequest: { schedule_id: 'created-manual', next_run_at: '2030-01-01T00:00:00Z' },
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
      { schedule_id: 'auto', name: 'Automatic schedule', run_type: 'auto', enabled: true, schedule_kind: 'interval', schedule_expr: '10m', environment_status: 'approved' },
      { schedule_id: 'manual', name: 'Manual schedule', run_type: 'manual', enabled: true, schedule_kind: 'interval', schedule_expr: '10m' },
    ], server_timezone: 'UTC' },
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
  const task = { task_id: 'task-one', task_no: 1, task_type: 'task', title: 'Keep all task content', priority: 'medium', status: 'todo', created_by: 'author', comment_count: 0 };
  const boardAccess = access({ areas: PROJECT_AREAS.map((area) => ({ area, enabled: true, level: area === 'board' ? 'write' : 'none' })) });
  await mount(boardAccess, {
    projectStudioTasksListRequest: { tasks: [task], total: 1 },
    projectStudioTaskGetRequest: { detail: { info: task, description_md: 'Existing content', attachments: [], comments: [] } },
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
  assert.ok(document.querySelector('#ps-task-desc').hasAttribute('disabled'));
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

test('editing a function saves one complete row of the matrix in one mutation', async () => {
  await mount(access({ project_admin: true, can_manage_members: true }), { projectStudioFunctionSaveRequest: { ok: true } });
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
