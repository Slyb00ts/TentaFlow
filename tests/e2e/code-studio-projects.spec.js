// =============================================================================
// File: tests/e2e/code-studio-projects.spec.js
// Description: The Code Studio ↔ Project Studio link (plan §20). A project that
//              is linked to a workspace may read the repository — but only as a
//              PINNED COMMIT, never as a live path on the owner node, because
//              repeatability is the whole point: a test run must be able to say
//              which code it tested.
//
//              Verified here:
//                • a workspace can be linked to a project and the link is listed
//                  from both ends,
//                • a linked project sees the repository tree of a pinned commit,
//                • the listing carries NO host path (leaking one would make the
//                  project depend on the owner node's layout),
//                • unlinking removes the access again.
// =============================================================================

const { test, expect } = require('@playwright/test');
const {
  startBinary, stopBinary, waitForServer, binaryExists, baseUrl,
} = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { startScriptedModel, helloWorldScript } = require('./helpers/scripted-model');

const PORT = 18113;
const DB = '/tmp/e2e-code-studio-proj.db';
const WORKSPACE = `link-${Date.now().toString(36)}`;
const PROJECT = `Projekt ${Date.now().toString(36)}`;

let proc;
let model;
let workspaceId = '';
let projectId = '';

test.describe.configure({ mode: 'serial' });

test.beforeAll(async () => {
  if (!binaryExists()) test.skip(true, 'tentaflow binary not built');
  // No agent turn here, but the workspace still needs a provider to exist for
  // the wizard to complete cleanly.
  model = startScriptedModel({ script: helloWorldScript() });
  proc = startBinary({ port: PORT, db: DB, rustLog: process.env.RUST_LOG ?? 'warn' });
  await waitForServer(PORT);
});

test.afterAll(async () => {
  stopBinary(proc);
  model?.stop();
  await new Promise((r) => setTimeout(r, 1500));
});

async function api(page, action, payload) {
  return page.evaluate(async ([a, p]) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return p === null ? ApiBinary.one(a) : ApiBinary.action(a, p);
  }, [action, payload === undefined ? null : payload]);
}

test.describe('Code Studio ↔ Projekty', () => {
  test('zakłada workspace i projekt', async ({ page }) => {
    test.setTimeout(180_000);
    await loginAsAdmin(page, { port: PORT });

    const me = await api(page, 'authMeRequest');
    const users = await api(page, 'usersListRequest');
    const userId = (users?.users ?? [])
      .find((u) => (u.username ?? '') === (me?.username ?? 'admin'))?.id;
    expect(userId).toBeTruthy();

    // Both modules gate creation behind a per-user grant.
    await api(page, 'codeStudioWorkspaceCreatorGrantSetRequest', { userId, granted: true });
    await api(page, 'projectStudioCreatorGrantSetRequest', { userId, granted: true })
      .catch(() => null);

    await page.goto(`${baseUrl(PORT)}/`);
    await page.locator('[data-view="apps-home"]').first().click();
    await page.locator('[data-route="code-studio"]').first().click();
    await page.waitForSelector('#cs-new, #cs-empty-new, #cs-table-host', { timeout: 30_000 });
    await page.locator('#cs-new, #cs-empty-new').first().click();
    await page.locator('#cs-wz-name input').first().fill(WORKSPACE);
    await page.locator('[data-action="next"]').first().click();
    await page.locator('[data-action="next"]').first().click();
    await page.locator('[data-action="next"]').first().click();

    await expect.poll(async () => {
      const body = await api(page, 'codeStudioWorkspacesListRequest', {});
      const ws = (body?.workspaces ?? []).find((w) => w.name === WORKSPACE);
      if (ws) workspaceId = ws.id ?? ws.workspaceId ?? workspaceId;
      return ws?.status ?? 'missing';
    }, { timeout: 90_000, message: 'workspace never reached active' }).toBe('active');

    // `template` and `modules` are validated against fixed sets
    // (project_studio/mod.rs:40-41) — an empty template is refused.
    const created = await api(page, 'projectStudioProjectCreateRequest', {
      name: PROJECT,
      description: 'Projekt testujący kod z workspace Code Studio.',
      template: 'tests',
      modules: ['knowledge', 'tests'],
      members: [],
    });
    projectId = created?.projectId ?? created?.project_id ?? created?.project?.id ?? '';
    expect(projectId, 'project was not created').toBeTruthy();
  });

  test('łączy workspace z projektem i widzi link z obu stron', async ({ page }) => {
    await loginAsAdmin(page, { port: PORT });

    const linked = await api(page, 'codeStudioProjectLinkSetRequest', {
      workspaceId, projectId, linked: true,
    });
    const links = linked?.links ?? [];
    expect(links.map((l) => l.projectId ?? l.project_id), 'link not reported back')
      .toContain(projectId);

    // …and the listing agrees on a fresh call.
    const listed = await api(page, 'codeStudioProjectLinkListRequest', { workspaceId });
    expect((listed?.links ?? []).map((l) => l.projectId ?? l.project_id))
      .toContain(projectId);
    const entry = (listed?.links ?? [])[0];
    expect(entry.projectName ?? entry.project_name, 'link carries no project name')
      .toBeTruthy();
  });

  test('projekt czyta repozytorium po PRZYPIĘTYM commicie, bez ścieżek hosta', async ({ page }) => {
    await loginAsAdmin(page, { port: PORT });

    // Git reads are session-scoped, so open one to reach the log. The commit it
    // reports is the provisioning commit — exactly what a project would pin.
    await page.goto(`${baseUrl(PORT)}/`);
    await page.locator('[data-view="apps-home"]').first().click();
    await page.locator('[data-route="code-studio"]').first().click();
    await page.locator(`text=${WORKSPACE}`).first().click();
    await page.locator('#cs-open-session:not([disabled])').waitFor({ timeout: 60_000 });
    await page.locator('#cs-open-session').click();
    await page.locator('#cs-sess-title input').first().waitFor({ timeout: 30_000 });
    await page.locator('#cs-sess-title input').first().fill('link e2e');
    await page.locator('[data-action="create"]').first().click();
    await page.waitForSelector('#cs-session-view', { state: 'visible', timeout: 30_000 });

    let sessionId = '';
    await expect.poll(async () => {
      const list = await api(page, 'codeStudioSessionsListRequest', { workspaceId })
        .catch((e) => ({ err: String(e) }));
      if (!sessionId) console.log('[diag] sessions:', JSON.stringify(list).slice(0, 300));
      const first = (list?.sessions ?? [])[0] ?? {};
      sessionId = first.sessionId ?? first.session_id ?? first.id ?? '';
      return sessionId;
    }, { timeout: 30_000, message: 'no session was created for the link test' })
      .toBeTruthy();

    const log = await api(page, 'codeStudioGitLogRequest', { workspaceId, sessionId, limit: 5 })
      .catch((e) => ({ err: String(e) }));
    console.log('[diag] git log:', JSON.stringify(log).slice(0, 300));
    const commit = (log?.commits ?? [])[0];
    expect(commit, 'the workspace has no commit to pin').toBeTruthy();
    const commitId = commit.id ?? commit.commitId ?? commit.commit_id ?? commit.oid;

    // The field is `commit` (protocol/code_studio.rs:1551), and the project is
    // identified separately — visibility comes from the link, not from the caller.
    const tree = await api(page, 'codeStudioRepoTreeRequest', {
      workspaceId, projectId, commit: commitId, pathPrefix: '', limit: 200,
    }).catch((e) => ({ err: String(e) }));
    console.log('[diag] repo tree:', JSON.stringify(tree).slice(0, 500));
    expect(tree?.err, 'a linked project could not read the pinned tree').toBeFalsy();

    const entries = tree?.entries ?? [];
    expect(Array.isArray(entries), 'no entry list in the response').toBe(true);

    // §20: names only. A host path here would tie the project to the owner
    // node's layout and break the "addressed by commit id" promise.
    const serialised = JSON.stringify(tree);
    expect(serialised, 'the repo listing leaks a host path')
      .not.toMatch(/\/home\/|\/tmp\/|code-studio\/[0-9a-f-]{36}\//);
  });

  test('rozłączenie odbiera dostęp', async ({ page }) => {
    await loginAsAdmin(page, { port: PORT });

    const after = await api(page, 'codeStudioProjectLinkSetRequest', {
      workspaceId, projectId, linked: false,
    });
    expect((after?.links ?? []).map((l) => l.projectId ?? l.project_id))
      .not.toContain(projectId);

    const sessions = await api(page, 'codeStudioSessionsListRequest', { workspaceId });
    const s0 = (sessions?.sessions ?? [])[0] ?? {};
    const sid = s0.sessionId ?? s0.session_id ?? s0.id ?? '';
    const log = await api(page, 'codeStudioGitLogRequest', { workspaceId, sessionId: sid, limit: 1 })
      .catch(() => null);
    const commitId = (log?.commits ?? [])[0]?.id;
    if (commitId) {
      const denied = await api(page, 'codeStudioRepoTreeRequest', {
        workspaceId, projectId, commit: commitId, pathPrefix: '', limit: 200,
      }).catch((e) => ({ err: String(e) }));
      console.log('[diag] tree after unlink:', JSON.stringify(denied).slice(0, 300));
      expect(denied?.err, 'an unlinked project can still read the repository').toBeTruthy();
    }
  });
});
