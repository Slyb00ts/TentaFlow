// =============================================================================
// File: tests/e2e/code-studio-harness.spec.js
// Description: End-to-end check of the WHOLE Code Studio harness, driven from
//              the browser exactly like an operator drives it: create a
//              workspace, open a session, tell the agent to write a program,
//              watch the tool calls land, then review and commit the result.
//
//              The model is scripted (helpers/scripted-model.js) so the turn is
//              deterministic. Everything BELOW the model is real: the flow
//              engine runs the code harness, tool_exec dispatches core.* tools,
//              the PEP gates them, the file is really written into the session
//              worktree, python really runs it, the patch set is real and the
//              commit is assembled by the git broker from the accepted blobs.
//
//              A failure here means the harness is broken, not the model.
// =============================================================================

const { test, expect } = require('@playwright/test');
const {
  startBinary,
  stopBinary,
  waitForServer,
  binaryExists,
  baseUrl,
} = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { startScriptedModel, helloWorldScript } = require('./helpers/scripted-model');

const PORT = 18111;
const DB = '/tmp/e2e-code-studio.db';
const WORKSPACE = `harness-${Date.now().toString(36)}`;
const PROGRAM = 'hello.py';
const PRINTED = 'Witaj z Code Studio';

// A model turn plus a python run is slower than a UI click; give the agent
// room without letting a wedged harness hang the suite.
const TURN_TIMEOUT = 90_000;

let proc;
let model;
// Resolved as the suite walks forward; the scoped protocol calls need both.
let workspaceId = '';
let sessionId = '';

test.describe.configure({ mode: 'serial' });

test.beforeAll(async () => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built — run cargo build (debug is enough)');
  }
  model = startScriptedModel({ script: helloWorldScript({ path: PROGRAM, message: PRINTED }) });
  proc = startBinary({ port: PORT, db: DB, rustLog: process.env.RUST_LOG ?? 'warn' });
  await waitForServer(PORT);
});

test.afterAll(async () => {
  stopBinary(proc);
  model?.stop();
  await new Promise((r) => setTimeout(r, 1500));
});

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// Calls the binary protocol from page context through the same shim the
// dashboard uses, so the test exercises the real wire rather than a side door.
// Unit requests (`meshNodeListRequest`) encode as (correlationId, sequence) and
// must go through `one()`; passing them a payload shifts the sequence argument
// and the codec dies converting an object to BigInt.
async function api(page, action, payload) {
  return page.evaluate(async ([a, p]) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return p === null ? ApiBinary.one(a) : ApiBinary.action(a, p);
  }, [action, payload === undefined ? null : payload]);
}

const scope = () => ({ workspaceId, sessionId });

// Registers the scripted endpoint as an `openai-compatible` external provider —
// the same call the deploy wizard makes (engine-deploy-wizard.js:3379).
async function registerScriptedProvider(page) {
  const nodes = await api(page, 'meshNodeListRequest');
  const first = (nodes?.nodes ?? [])[0] ?? {};
  const nodeId = first.nodeId ?? first.node_id ?? '';

  const res = await api(page, 'serviceManifestDeployRequest', {
    engineId: 'openai-compatible',
    deployMethod: 'external',
    nodeId,
    configJson: JSON.stringify({
      base_url: model.baseUrl,
      api_key: 'harness-test-key',
      auth_mode: 'api',
      model_repo: 'harness-test',
    }),
  });
  expect(res?.deployId || res?.deploy_id, 'provider deploy returned no id').toBeTruthy();
}

async function gotoCodeStudio(page) {
  // Deep link: the router now reads `#/screen` at boot and on hashchange, so a
  // pasted URL opens the screen it names instead of the dashboard. Before the
  // fix the hash was decoration and every test had to click through All apps.
  await page.goto(`${baseUrl(PORT)}/#/code-studio`);
  const direct = page.locator('#cs-new, #cs-empty-new, #cs-table-host').first();
  if (await direct.count().then((n) => n > 0).catch(() => false)) {
    await direct.waitFor({ timeout: 30_000 });
    return;
  }
  return gotoCodeStudioViaTile(page);
}

async function gotoCodeStudioViaTile(page) {
  // The shell ignores the URL hash (app.js ends with `Router.init('dashboard')`
  // and router.js has no hashchange listener), and navigating too early loses
  // the race with that init. So take the path a user takes: All apps → the
  // Code Studio tile. It also proves the tile is reachable at all.
  await page.goto(`${baseUrl(PORT)}/`);
  await page.locator('[data-view="apps-home"]').first().click();
  await page.locator('[data-route="code-studio"]').first().click();
  await page.waitForSelector('#cs-new, #cs-empty-new, #cs-table-host', { timeout: 30_000 });
}



// Re-opens the workspace and its session; every test gets a fresh page.
async function openSession(page) {
  await gotoCodeStudio(page);
  await page.locator(`text=${WORKSPACE}`).first().click();

  // `#cs-open-session` opens a dialog that CREATES a session; on re-entry we
  // want the one we already have, otherwise every test would start a new run.
  const existing = page.locator('[data-open-session]').first();
  if (await existing.count()) {
    await existing.click();
  } else {
    await page.locator('#cs-open-session').click();
    await page.locator('#cs-sess-title input').first().fill('harness e2e');
    await page.locator('[data-action="create"]').first().click();
  }
  await page.waitForSelector('#cs-session-view', { state: 'visible', timeout: 30_000 });

  // Keep the session we already know: re-deriving it from the listing picks
  // whatever sits first, and a second session created along the way would send
  // the later assertions to an empty one.
  if (!sessionId) {
    const list = await api(page, 'codeStudioSessionsListRequest', { workspaceId });
    const sessions = list?.sessions ?? [];
    sessionId = sessions[0]?.id ?? sessions[0]?.sessionId ?? '';
  }
  expect(sessionId, 'no session id after opening the session view').toBeTruthy();
}



// Answers every pending approval the way an operator would. Autonomy `normal`
// gates fs_write and exec, so a turn simply parks until somebody decides — the
// PEP working as designed (plan §9.3). `allow_for_run` keeps the grant scoped
// to this run instead of leaking into the workspace allowlist.
async function approvePending(page, { deadlineMs = 60_000, decision = 'allow_for_run' } = {}) {
  const until = Date.now() + deadlineMs;
  let decided = 0;
  while (Date.now() < until) {
    const body = await api(page, 'codeStudioApprovalsListRequest', scope()).catch(() => null);
    const pending = (body?.approvals ?? []).filter((a) => (a.status ?? '') === 'pending');
    for (const a of pending) {
      await api(page, 'codeStudioApprovalDecideRequest', {
        ...scope(),
        approvalId: a.id ?? a.approvalId ?? a.approval_id,
        decision,
      });
      decided += 1;
    }
    if (decided) return decided;
    await page.waitForTimeout(1000);
  }
  return decided;
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

test.describe('Code Studio — cały harness od promptu do commita', () => {
  test('rejestruje skryptowany model jako dostawcę zewnętrznego', async ({ page }) => {
    await loginAsAdmin(page, { port: PORT });
    await registerScriptedProvider(page);

    // The provider must become reachable, otherwise the agent turn later fails
    // for a reason that has nothing to do with the harness.
    await expect.poll(async () => {
      const body = await api(page, 'serviceListRequest');
      const svc = (body?.services ?? []).find(
        (s) => (s.engineId ?? s.engine_id) === 'openai-compatible',
      );
      return svc?.status ?? 'missing';
    }, { timeout: 60_000, message: 'scripted provider never came up' }).toMatch(/running|degraded/);
  });

  test('świeża instalacja: agent rusza bez ręcznego przypisania modelu', async ({ page }) => {
    await loginAsAdmin(page, { port: PORT });

    // Regression for the "first turn of a fresh install dies" defect: the seed
    // leaves every code agent with model = NULL and the llm block carries none
    // of its own, so before the fix the first session failed with
    //   "llm adapter: no model — node config 'model' nor envelope.meta['model']"
    // agent_context now falls back to the platform default, so we deliberately
    // do NOT bind a model here — the turn in the next test must work anyway.
    const listResp = await api(page, 'agentsListRequest', {});
    const agents = JSON.parse(listResp?.agentsJson ?? listResp?.agents_json ?? '[]');
    const codeAgents = agents.filter((a) => String(a.name ?? '').startsWith('code-'));
    expect(codeAgents.length, 'no seeded code-* agents').toBeGreaterThan(0);
    for (const a of codeAgents) {
      expect(a.model ?? '', `${a.name} unexpectedly ships with a model`).toBe('');
    }

    // …and a model IS available on the node, which is what the fallback picks.
    const models = await api(page, 'modelListRequest');
    expect((models?.models ?? []).length, 'no model on the node to fall back to')
      .toBeGreaterThan(0);
  });

  test('zakłada workspace przez kreator', async ({ page }) => {
    await loginAsAdmin(page, { port: PORT });

    // Creating a workspace is gated by `code_studio_creator_grants` (plan §5.1):
    // org_admin may grant it, but nobody holds it in a fresh database. Without
    // this the wizard button renders disabled and the test would fail on a
    // product rule rather than on the harness.
    // authMeRequest carries user_id as [u8; 16], which does not survive as the
    // string the grant handler wants. The user listing already exposes ids in
    // the form every other Code Studio call uses, so resolve it there.
    const me = await api(page, 'authMeRequest');
    const users = await api(page, 'usersListRequest');
    const mine = (users?.users ?? []).find(
      (u) => (u.username ?? '') === (me?.username ?? 'admin'),
    );
    const userId = mine?.id ?? mine?.userId ?? mine?.user_id ?? '';
    expect(userId, 'could not resolve the admin user id').toBeTruthy();
    await api(page, 'codeStudioWorkspaceCreatorGrantSetRequest', { userId, granted: true });

    await gotoCodeStudio(page);

    // Empty state and populated list offer the same action under two ids.
    await page.locator('#cs-new, #cs-empty-new').first().click();
    await page.locator('#cs-wz-name input').first().fill(WORKSPACE);

    // Step 1 → 2 (execution mode). trusted_native is preselected and is what we
    // want: the e2e box has no container runtime guarantee.
    await page.locator('[data-action="next"]').first().click();
    await page.locator('[data-action="next"]').first().click();

    // Step 3: source stays "empty" — provisioning creates the initial commit,
    // so the workspace has a base_commit without touching the network. The
    // footer keeps a single forward button: on the last step `next` relabels to
    // "Create workspace" but the action stays the same (code-studio.js:1159).
    await page.locator('[data-action="next"]').first().click();

    // Provisioning is a saga; the row only turns active at its last step.
    await expect.poll(async () => {
      const body = await api(page, 'codeStudioWorkspacesListRequest', {});
      const ws = (body?.workspaces ?? []).find((w) => w.name === WORKSPACE);
      if (ws) workspaceId = ws.id ?? ws.workspaceId ?? workspaceId;
      return ws?.status ?? 'missing';
    }, { timeout: 60_000, message: 'workspace never reached active' }).toBe('active');

    expect(workspaceId, 'workspace id not resolved').toBeTruthy();
  });

  test('agent pisze i uruchamia program, zmiana trafia do przeglądu', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 90_000);
    await loginAsAdmin(page, { port: PORT });
    await openSession(page);

    // Talk to the agent exactly like a user would.
    const composer = page.locator('#cs-session-view textarea').first();
    await composer.fill(`Napisz program ${PROGRAM}, który wypisuje „${PRINTED}", i uruchom go.`);
    await composer.press('Enter');

    const view = page.locator('#cs-session-view');

    // Give the turn a moment, then dump state unconditionally. A conditional
    // dump is useless here: if the run has not settled yet there is nothing to
    // report, and by the time the UI assertion fails the test is already over.
    await page.waitForTimeout(20_000);
    const runs = await api(page, 'codeStudioSessionRunsRequest', scope()).catch((e) => ({ err: String(e) }));
    console.log('[diag] runs:', JSON.stringify(runs));
    console.log('[diag] model calls:', model.calls.length);
    if (model.calls.length) {
      const first = model.calls[0];
      console.log('[diag] first call model:', first.model, 'tools:', (first.tools ?? []).length);
    }
    const tl = await api(page, 'codeStudioSessionTimelineRequest', scope()).catch(() => null);
    console.log('[diag] timeline tail:', JSON.stringify((tl?.events ?? tl?.items ?? []).slice(-8)));

    // 1) The harness really called the tools — these rows come from tool_exec,
    //    not from the model's prose.
    await expect(view.getByText('core.fs_write', { exact: false }).first())
      .toBeVisible({ timeout: TURN_TIMEOUT });
    // The write parks on an approval; answer it so the turn can continue.
    const firstDecisions = await approvePending(page);
    expect(firstDecisions, 'no approval was raised for fs_write').toBeGreaterThan(0);

    await expect(view.getByText('core.exec', { exact: false }).first())
      .toBeVisible({ timeout: TURN_TIMEOUT });

    // …and exec asks too, under autonomy `normal`.
    await approvePending(page);

    // 2) The program really ran: python's stdout came back through the exec
    //    tool. This is the strongest single signal that the file exists on disk
    //    in the session worktree and is valid python.
    await expect(view.getByText(PRINTED, { exact: false }).first())
      .toBeVisible({ timeout: TURN_TIMEOUT });

    // 3) The write produced a patch set instead of a silent commit.
    // The list carries summaries (PatchSetInfo has file_count, not files); the
    // paths live in the detail response, so walk both.
    await expect.poll(async () => {
      const body = await api(page, 'codeStudioPatchSetsListRequest', scope());
      const sets = body?.patchSets ?? body?.patch_sets ?? [];
      const paths = [];
      for (const info of sets) {
        const id = info.patchSetId ?? info.patch_set_id;
        const detail = await api(page, 'codeStudioPatchSetGetRequest', {
          ...scope(), patchSetId: id,
        }).catch(() => null);
        for (const f of detail?.files ?? []) paths.push(f.path);
      }
      return paths;
    }, { timeout: TURN_TIMEOUT, message: 'no patch set for the written file' })
      .toContain(PROGRAM);
  });

  test('przegląd akceptuje zmianę, a commit powstaje z zaakceptowanych blobów', async ({ page }) => {
    test.setTimeout(150_000);
    await loginAsAdmin(page, { port: PORT });
    await openSession(page);

    const before = await api(page, 'codeStudioGitLogRequest', { ...scope(), limit: 50 });
    const beforeCount = (before?.commits ?? []).length;

    // Accept the review through the very call the Changes pane makes
    // (code-studio-panes.js:436). The hunk buttons live in tf-diff's shadow DOM
    // and are covered by its own unit tests; what matters here is the wire
    // contract and what git does with an accepted set.
    const sets = await api(page, 'codeStudioPatchSetsListRequest', scope());
    const all = sets?.patchSets ?? sets?.patch_sets ?? [];
    console.log('[diag] patch sets:', JSON.stringify(all.map((x) => ({
      id: x.patchSetId ?? x.patch_set_id, status: x.status, scope: x.scope,
      files: x.fileCount ?? x.file_count,
    }))));
    const target = all.find((s2) => ['open', 'in_review'].includes(s2.status ?? '')) ?? all[0];
    expect(target, 'no patch set at all').toBeTruthy();
    const patchSetId = target.patchSetId ?? target.patch_set_id;

    const detail = await api(page, 'codeStudioPatchSetGetRequest', { ...scope(), patchSetId });
    const files = (detail?.files ?? []).map((f) => ({
      patchFileId: f.patch_file_id ?? f.patchFileId,
      decision: 'accept',
      hunks: (f.hunks ?? []).map((h) => ({
        patchHunkId: h.patch_hunk_id ?? h.patchHunkId,
        decision: 'accept',
      })),
    }));
    expect(files.length, 'the patch set carries no files').toBeGreaterThan(0);

    await api(page, 'codeStudioPatchDecideRequest', { ...scope(), patchSetId, files });

    // Accepting a review does NOT commit on its own: `commit_accepted_blobs` is
    // reached from the git_commit path (dispatch/code_studio.rs:4759), which
    // then picks up the already-accepted set. So ask for the commit the way the
    // operator does — this is also what proves §11.5, because the commit is
    // assembled from the accepted blobs rather than from the worktree.
    // Under autonomy `normal` the commit itself is gated too: the first call
    // comes back as `approval_required` (a Conflict, not a failure). Answer it
    // and ask again — that is the operator's path, and it proves the gate.
    const commitOnce = () => api(page, 'codeStudioGitCommitRequest', {
      ...scope(),
      message: 'feat: add hello.py\n\nAccepted in review by the e2e operator.',
    });

    const first = await commitOnce().catch((e) => ({ err: String(e) }));
    if (first?.err) {
      expect(first.err, 'commit failed for a reason other than the approval gate')
        .toContain('approval_required');
      // `allow_for_run` is scoped to an agent run; this commit is operator-
      // initiated and belongs to none. The server now REFUSES that combination
      // instead of storing a grant that binds to nothing — assert the refusal,
      // then answer with a scope that actually applies.
      const refused = await api(page, 'codeStudioApprovalsListRequest', scope())
        .then((b) => (b?.approvals ?? []).find((a) => (a.status ?? '') === 'pending'))
        .then((a) => api(page, 'codeStudioApprovalDecideRequest', {
          ...scope(),
          approvalId: a.id ?? a.approvalId ?? a.approval_id,
          decision: 'allow_for_run',
        }).catch((e) => String(e)));
      expect(String(refused), 'a run-scoped grant with no run was accepted')
        .toContain('bind to nothing');

      const decided = await approvePending(page, { decision: 'always' });
      expect(decided, 'the commit gate raised no approval to answer').toBeGreaterThan(0);
      await commitOnce();
    }

    // git_commit is systemowe: the coordinator builds the commit from the
    // accepted blobs. A new commit on top of the provisioning one is the whole
    // promise of §11.5 — what was reviewed is what was committed.
    await expect.poll(async () => {
      const body = await api(page, 'codeStudioGitLogRequest', { ...scope(), limit: 50 });
      return (body?.commits ?? []).length;
    }, { timeout: 90_000, message: 'nothing was committed after accepting the review' })
      .toBeGreaterThan(beforeCount);
  });

  test('model dostał katalog narzędzi i wyniki z powrotem w pętli', async () => {
    // The harness must have fed the tool results back, otherwise the loop is
    // not a loop. Three calls at minimum: two tool turns plus the final answer.
    expect(model.calls.length, 'agent loop did not iterate').toBeGreaterThanOrEqual(3);

    const first = model.calls[0];
    const toolNames = (first.tools ?? []).map((t) => t.function?.name ?? t.name);
    expect(toolNames, 'the model was offered no file tools').toContain('core.fs_write');

    const last = model.calls[model.calls.length - 1];
    const roles = (last.messages ?? []).map((m) => m.role);
    expect(roles, 'tool results never came back into the conversation').toContain('tool');
  });
});
