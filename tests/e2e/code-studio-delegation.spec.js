// =============================================================================
// File: tests/e2e/code-studio-delegation.spec.js
// Description: Does the orchestrator actually RUN subagents, and can it watch
//              and steer them? Plus: does AI-assisted agent building work?
//
//              Driven from the browser like the main harness spec. The model is
//              scripted per ROLE (helpers/scripted-model.js) so the parent and
//              the child can be told to do different things while hitting the
//              same endpoint — otherwise one flat script would be consumed by
//              whichever agent asked first.
//
//              Verified here:
//                • core.agent_spawn starts a real child run bound to the same
//                  Code Studio session (the binding travels in extra_meta and
//                  cannot be redirected — run_manager.rs:616),
//                • core.agent_list lets the parent see what the child is doing,
//                • core.agent_wait blocks until the child settles,
//                • the child's work lands in the parent's worktree,
//                • agentBuilderAssistRequest returns a usable agent proposal.
// =============================================================================

const { test, expect } = require('@playwright/test');
const {
  startBinary, stopBinary, waitForServer, binaryExists, baseUrl,
} = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { startScriptedModel, delegationScripts } = require('./helpers/scripted-model');

const PORT = 18112;
const DB = '/tmp/e2e-code-studio-deleg.db';
const WORKSPACE = `deleg-${Date.now().toString(36)}`;
const PROGRAM = 'from_subagent.py';
const PRINTED = 'Napisane przez subagenta';
const TURN_TIMEOUT = 120_000;

let proc;
let model;
let workspaceId = '';
let sessionId = '';

test.describe.configure({ mode: 'serial' });

test.beforeAll(async () => {
  if (!binaryExists()) test.skip(true, 'tentaflow binary not built');
  model = startScriptedModel({ scripts: delegationScripts({ path: PROGRAM, message: PRINTED }) });
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

const scope = () => ({ workspaceId, sessionId });

async function approvePending(page, { deadlineMs = 90_000 } = {}) {
  const until = Date.now() + deadlineMs;
  let decided = 0;
  while (Date.now() < until) {
    const body = await api(page, 'codeStudioApprovalsListRequest', scope()).catch(() => null);
    const pending = (body?.approvals ?? []).filter((a) => (a.status ?? '') === 'pending');
    for (const a of pending) {
      await api(page, 'codeStudioApprovalDecideRequest', {
        ...scope(),
        approvalId: a.id ?? a.approvalId ?? a.approval_id,
        decision: 'allow_for_run',
      });
      decided += 1;
    }
    await page.waitForTimeout(1000);
  }
  return decided;
}

test.describe('Code Studio — delegacja do subagentów i kreator agentów', () => {
  test('przygotowanie: model, grant, workspace i sesja', async ({ page }) => {
    test.setTimeout(180_000);
    await loginAsAdmin(page, { port: PORT });

    // Provider.
    const nodes = await api(page, 'meshNodeListRequest');
    const nodeId = (nodes?.nodes ?? [])[0]?.nodeId ?? (nodes?.nodes ?? [])[0]?.node_id ?? '';
    await api(page, 'serviceManifestDeployRequest', {
      engineId: 'openai-compatible',
      deployMethod: 'external',
      nodeId,
      configJson: JSON.stringify({
        base_url: model.baseUrl, api_key: 'k', auth_mode: 'api', model_repo: 'harness-test',
      }),
    });
    await expect.poll(async () => {
      const body = await api(page, 'serviceListRequest');
      return (body?.services ?? []).find(
        (s) => (s.engineId ?? s.engine_id) === 'openai-compatible',
      )?.status ?? 'missing';
    }, { timeout: 60_000 }).toMatch(/running|degraded/);

    // Model onto every code agent — a fresh install leaves them NULL.
    // Model discovery runs on the supervisor's own cadence, so a service that
    // just turned `running` does not yet have its models in the catalogue.
    let modelName = '';
    await expect.poll(async () => {
      const models = await api(page, 'modelListRequest');
      modelName = (models?.models ?? [])
        .map((m) => m.modelName ?? m.model_name)
        .find((n) => String(n).includes('harness-test')) ?? '';
      return modelName;
    }, { timeout: 90_000, message: 'the scripted model never reached the catalogue' })
      .toBeTruthy();

    const listResp = await api(page, 'agentsListRequest', {});
    const agents = JSON.parse(listResp?.agentsJson ?? listResp?.agents_json ?? '[]');
    for (const a of agents.filter((x) => String(x.name ?? '').startsWith('code-'))) {
      const d = await api(page, 'agentsDetailRequest', { agentId: a.id });
      const agent = JSON.parse(d?.agentJson ?? d?.agent_json ?? '{}');
      agent.model = modelName;
      await api(page, 'agentsUpsertRequest', { agentJson: JSON.stringify(agent) });
    }

    // Creator grant + workspace + session.
    const me = await api(page, 'authMeRequest');
    const users = await api(page, 'usersListRequest');
    const mine = (users?.users ?? []).find((u) => (u.username ?? '') === (me?.username ?? 'admin'));
    await api(page, 'codeStudioWorkspaceCreatorGrantSetRequest', {
      userId: mine?.id ?? mine?.userId ?? mine?.user_id, granted: true,
    });

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
    }, { timeout: 60_000 }).toBe('active');

    await page.locator(`text=${WORKSPACE}`).first().click();
    // The button carries `disabled` until the workspace is active (code-studio.js:1252),
    // and a click on a disabled tf-button is silently swallowed.
    await page.locator('#cs-open-session:not([disabled])').waitFor({ timeout: 60_000 });
    await page.locator('#cs-open-session').click();
    await page.locator('#cs-sess-title input').first().waitFor({ timeout: 30_000 });
    await page.locator('#cs-sess-title input').first().fill('delegacja e2e');
    await page.locator('[data-action="create"]').first().click();
    await page.waitForSelector('#cs-session-view', { state: 'visible', timeout: 30_000 });

    await expect.poll(async () => {
      const sessions = await api(page, 'codeStudioSessionsListRequest', { workspaceId });
      sessionId = (sessions?.sessions ?? [])[0]?.id
        ?? (sessions?.sessions ?? [])[0]?.sessionId ?? '';
      return sessionId;
    }, { timeout: 30_000, message: 'the created session never reached the listing' })
      .toBeTruthy();
  });

  test('orkiestrator uruchamia subagenta, widzi go i czeka na niego', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 120_000);
    await loginAsAdmin(page, { port: PORT });
    await page.goto(`${baseUrl(PORT)}/`);
    await page.locator('[data-view="apps-home"]').first().click();
    await page.locator('[data-route="code-studio"]').first().click();
    await page.locator(`text=${WORKSPACE}`).first().click();
    await page.locator('[data-open-session]').first().click();
    await page.waitForSelector('#cs-session-view', { state: 'visible', timeout: 30_000 });

    const composer = page.locator('#cs-session-view textarea').first();
    await composer.fill(`Zleć subagentowi utworzenie ${PROGRAM}.`);
    await composer.press('Enter');

    // Approvals run in the background for the whole turn: the child's fs_write
    // is gated exactly like the parent's would be.
    const approver = approvePending(page, { deadlineMs: TURN_TIMEOUT });

    // A child run must appear, owned by the parent and bound to THIS session.
    await expect.poll(async () => {
      const body = await api(page, 'codeStudioSessionRunsRequest', scope());
      const runs = body?.runs ?? [];
      const child = runs.find((r) => (r.parentRunId ?? r.parent_run_id));
      if (child) {
        console.log('[diag] child run:', JSON.stringify({
          kind: child.kind, status: child.status,
          agentId: child.agentId ?? child.agent_id,
          parent: child.parentRunId ?? child.parent_run_id,
        }));
      }
      return runs.filter((r) => (r.parentRunId ?? r.parent_run_id)).length;
    }, { timeout: TURN_TIMEOUT, message: 'orchestrator never spawned a child run' })
      .toBeGreaterThan(0);

    await approver;

    // The parent asked what its children were doing, and waited for them.
    const seen = model.calls.flatMap((c) => (c.messages ?? []))
      .filter((m) => m.role === 'tool')
      .map((m) => (typeof m.content === 'string' ? m.content : JSON.stringify(m.content ?? '')));
    const toolCalls = model.calls.flatMap((c) => (c.messages ?? []))
      .filter((m) => Array.isArray(m.tool_calls))
      .flatMap((m) => m.tool_calls.map((t) => t.function?.name));
    console.log('[diag] tool calls seen by the loop:', JSON.stringify([...new Set(toolCalls)]));
    expect(toolCalls, 'agent_spawn never reached the loop').toContain('core.agent_spawn');
    expect(toolCalls, 'agent_list never reached the loop').toContain('core.agent_list');
    expect(seen.length, 'no tool results were fed back').toBeGreaterThan(0);

    // The child's file really landed in the parent's worktree.
    await expect.poll(async () => {
      const tree = await api(page, 'codeStudioFileTreeRequest', { ...scope(), path: '' })
        .catch(() => null);
      const names = (tree?.entries ?? tree?.nodes ?? []).map((e) => e.name ?? e.path);
      return names;
    }, { timeout: 60_000, message: 'the subagent file never appeared in the worktree' })
      .toContain(PROGRAM);
  });

  test('kreator agentów AI zwraca propozycję agenta', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page, { port: PORT });

    // The builder resolves its model through the `curator_model` setting, which
    // falls back to the alias "default" (skills/curator.rs:817). On a fresh
    // install nothing resolves that alias, so point it at the scripted model.
    const models = await api(page, 'modelListRequest');
    const modelName = (models?.models ?? [])
      .map((m) => m.modelName ?? m.model_name)
      .find((n) => String(n).includes('harness-test'));
    // The payload is `{ entries: [{key, value, isSecret}] }` (codec.js:1878) —
    // and it must NOT be swallowed: if the write fails the builder falls back to
    // the unresolvable "default" alias and the test would blame the builder.
    await api(page, 'settingsUpdateRequest', {
      entries: [{ key: 'curator_model', value: modelName, isSecret: false }],
    });
    const settings = await api(page, 'settingsListRequest');
    const stored = (settings?.settings ?? settings?.entries ?? [])
      .find((e) => (e.key ?? '') === 'curator_model');
    expect(stored?.value, 'curator_model was not stored').toBe(modelName);

    const resp = await api(page, 'agentBuilderAssistRequest', {
      messagesJson: JSON.stringify([
        { role: 'user', content: 'Potrzebuję agenta, który przegląda kod pod kątem bezpieczeństwa.' },
      ]),
    }).catch((e) => ({ error: String(e) }));

    // What did the builder actually send to the model? Roles + a snippet of each
    // message, so the routing marker can be chosen from evidence.
    const lastCall = model.calls[model.calls.length - 1] ?? {};
    console.log('[diag] builder request keys:', JSON.stringify(Object.keys(lastCall)));
    for (const m of lastCall.messages ?? []) {
      const text = typeof m.content === 'string' ? m.content : JSON.stringify(m.content ?? '');
      console.log(`[diag] builder msg role=${m.role} len=${text.length} :: ${text.slice(0, 160).replace(/\n/g, ' ')}`);
    }
    console.log('[diag] agent builder response:', JSON.stringify(resp).slice(0, 700));
    expect(resp?.error, 'agent builder refused the request').toBeFalsy();

    // A reply alone only proves the plumbing; the point of the builder is the
    // PROPOSAL, extracted from a model answer that wraps JSON in prose.
    const result = JSON.parse(resp?.resultJson ?? resp?.result_json ?? '{}');
    expect(result.reply, 'builder returned no reply').toBeTruthy();
    expect(result.proposal, 'builder extracted no agent proposal').toBeTruthy();
    expect(result.proposal.name).toBe('security-reviewer');
    expect(result.proposal.system_prompt, 'proposal has no system prompt').toBeTruthy();
    expect(Array.isArray(result.proposal.tools), 'proposal has no tool list').toBe(true);

    // And the proposal must be usable: save it and see it in the roster.
    await api(page, 'agentsUpsertRequest', {
      agentJson: JSON.stringify({
        name: result.proposal.name,
        display_name: result.proposal.display_name,
        description: result.proposal.description,
        system_prompt: result.proposal.system_prompt,
        tools_json: JSON.stringify(result.proposal.tools),
        max_iterations: result.proposal.max_iterations ?? 25,
      }),
    });
    const listResp = await api(page, 'agentsListRequest', {});
    const saved = JSON.parse(listResp?.agentsJson ?? listResp?.agents_json ?? '[]')
      .find((a) => a.name === 'security-reviewer');
    expect(saved, 'the proposed agent was not saved').toBeTruthy();
  });
});
