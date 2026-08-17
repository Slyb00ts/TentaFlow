// =============================================================================
// File: tests/e2e/code-studio-pipeline.spec.js
// Description: Proof that the ENFORCED pipeline is enforced — that the promise
//              lives in the running system and not only in the seeded graph.
//
//              A new session is pinned to harness variant C, whose graph says:
//              planning argues with a critic before anything is built, an
//              implementer never works without a tester behind it, and a critic
//              judges the result against the original request. None of that is
//              the model's choice; it is the shape of the DAG.
//
//              So this spec asserts what the model was ASKED, not what it
//              answered: the scripted endpoint records every prompt it served,
//              and a pipeline that really ran must have consulted the planner,
//              the critic, the implementer and the tester. It also asserts the
//              cheap path — a turn that only answers a question must NOT drag
//              five sub-agents behind it.
// =============================================================================

const { test, expect } = require('@playwright/test');
const {
  startBinary, stopBinary, waitForServer, binaryExists, baseUrl,
} = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const {
  startScriptedModel, helloWorldScript, enforcedPipelineScripts, tool, say,
} = require('./helpers/scripted-model');

// An orchestrator turn that DOES work and then answers. Deliberately not
// `helloWorldScript`: that one ends by asking for a commit, which parks the run
// on the review gate — and a parked run never leaves the tool loop, so the
// pipeline behind it would never get its turn. Here the point is what happens
// AFTER a completed turn.
function workThenAnswer() {
  return [
    tool('core.workspace_info', {}),
    tool('core.fs_write', {
      path: 'hello.py',
      content: '#!/usr/bin/env python3\nprint("Witaj z Code Studio")\n',
      expected_sha256: '',
    }),
    tool('core.exec', { argv: ['python3', 'hello.py'] }),
    say('Napisałem hello.py i uruchomiłem go.'),
  ];
}

const PORT = 18115;
const DB = '/tmp/e2e-code-studio-pipeline.db';
const WORKSPACE = `potok-${Date.now().toString(36)}`;

let proc;
let model;
let workspaceId = '';

test.describe.configure({ mode: 'serial' });

test.beforeAll(async () => {
  if (!binaryExists()) test.skip(true, 'tentaflow binary not built');
  model = startScriptedModel({
    scripts: [
      { match: 'Jesteś agentem programistycznym', steps: workThenAnswer() },
      ...enforcedPipelineScripts(),
    ],
  });
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

// Which roles the endpoint has been asked to play, read off the system prompts
// it was actually sent. This is the observation that matters: a role appears
// here only because the GRAPH delegated to it.
function rolesConsulted() {
  const seen = new Set();
  const markers = {
    orkiestrator: 'Jesteś agentem programistycznym',
    planista: 'Jesteś planistą zmian w kodzie',
    krytyk: 'Jestes krytykiem',
    wykonawca: 'Piszesz kod',
    tester: 'Uruchamiasz testy i buildy',
  };
  for (const call of model.calls) {
    const system = (call.messages ?? [])
      .filter((m) => m.role === 'system')
      .map((m) => (typeof m.content === 'string' ? m.content : JSON.stringify(m.content ?? '')))
      .join('\n');
    for (const [role, marker] of Object.entries(markers)) {
      if (system.includes(marker)) seen.add(role);
    }
  }
  return seen;
}

async function openSession(page, title) {
  await page.goto(`${baseUrl(PORT)}/#/code-studio`);
  await page.locator(`text=${WORKSPACE}`).first().click();
  await page.locator('#cs-open-session:not([disabled])').waitFor({ timeout: 90_000 });
  await page.locator('#cs-open-session').click();
  await page.locator('#cs-sess-title input').first().fill(title);
  await page.locator('[data-action="create"]').first().click();
  await page.waitForSelector('#cs-session-view', { state: 'visible', timeout: 60_000 });
}

// Answers every permission question the agent raises, so a turn is not judged
// to have "not delegated" when it is really just parked on an approval.
async function answerFor(page, ms) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    const ask = page.locator('[data-answer]:not([hidden])').first();
    if (await ask.count().catch(() => 0) > 0) {
      const option = ask.locator('tf-option-row[data-action]:not([disabled])').first();
      if (await option.count().catch(() => 0) > 0) {
        await option.click({ timeout: 6000 }).catch(() => {});
        await page.waitForTimeout(1200);
        continue;
      }
    }
    await page.waitForTimeout(1500);
  }
}

test.describe('Code Studio — wymuszony potok', () => {
  test('zaklada workspace', async ({ page }) => {
    test.setTimeout(240_000);
    await loginAsAdmin(page, { port: PORT });

    const nodes = await api(page, 'meshNodeListRequest');
    await api(page, 'serviceManifestDeployRequest', {
      engineId: 'openai-compatible',
      deployMethod: 'external',
      nodeId: (nodes?.nodes ?? [])[0]?.nodeId ?? (nodes?.nodes ?? [])[0]?.node_id ?? '',
      configJson: JSON.stringify({
        base_url: model.baseUrl,
        api_key: 'pipeline-key',
        auth_mode: 'api',
        model_repo: 'harness-test',
      }),
    });

    const me = await api(page, 'authMeRequest');
    const users = await api(page, 'usersListRequest');
    const userId = (users?.users ?? [])
      .find((u) => (u.username ?? '') === (me?.username ?? 'admin'))?.id;
    await api(page, 'codeStudioWorkspaceCreatorGrantSetRequest', { userId, granted: true });

    await page.goto(`${baseUrl(PORT)}/#/code-studio`);
    await page.locator('#cs-new, #cs-empty-new').first().click();
    await page.locator('#cs-wz-name input').first().fill(WORKSPACE);
    for (let n = 0; n < 3; n += 1) {
      await page.locator('[data-action="next"]:visible').first().click({ timeout: 10_000 });
      await page.waitForTimeout(400);
    }
    await expect.poll(async () => {
      const body = await api(page, 'codeStudioWorkspacesListRequest', {});
      const ws = (body?.workspaces ?? []).find((w) => w.name === WORKSPACE);
      if (ws) workspaceId = ws.id ?? ws.workspaceId ?? workspaceId;
      return ws?.status ?? 'brak';
    }, { timeout: 150_000 }).toBe('active');
  });

  test('tura, ktora zmienia kod, przechodzi przez planiste, krytyka, wykonawce i testera',
    async ({ page }) => {
      test.setTimeout(420_000);
      await loginAsAdmin(page, { port: PORT });
      await openSession(page, 'potok');

      const box = page.locator('#cs-session-view textarea').first();
      await box.waitFor({ state: 'visible', timeout: 25_000 });
      await box.fill('Napisz prosty program w Pythonie i uruchom go.');
      await box.press('Enter');

      // The agent asks before it touches anything, and the pipeline only runs
      // once the turn completes — so keep answering while watching the roles
      // appear. Answering in a separate loop would race with the poll.
      const answering = answerFor(page, 240_000);
      await expect.poll(() => [...rolesConsulted()].sort().join(','), {
        timeout: 240_000,
        intervals: [3000],
        message: 'graf nie zwolal calego potoku',
      }).toBe('krytyk,orkiestrator,planista,tester,wykonawca');
      await answering.catch(() => {});

      // …and the delegation is visible as real sub-runs, not just as prompts:
      // one root run for the orchestrator plus a child per delegated role.
      const list = await api(page, 'codeStudioSessionsListRequest', { workspaceId });
      const first = (list?.sessions ?? [])[0] ?? {};
      const sessionId = first.sessionId ?? first.session_id ?? first.id ?? '';
      const runs = await api(page, 'codeStudioSessionRunsRequest', { workspaceId, sessionId })
        .catch(() => null);
      expect((runs?.runs ?? []).length, 'sesja nie pokazuje przebiegow subagentow')
        .toBeGreaterThan(1);
    });

  test('tura, ktora tylko odpowiada, nie ciagnie za soba potoku', async ({ page }) => {
    test.setTimeout(300_000);
    await loginAsAdmin(page, { port: PORT });

    // A fresh endpoint whose orchestrator answers WITHOUT calling any tool: the
    // condition block in the graph must then skip the whole review pipeline.
    // Read the port BEFORE closing: the getter reads it off a live server, and
    // a restart on a different port would leave the registered provider aimed
    // at nothing — which looks exactly like "the graph did not delegate".
    const port = model.port;
    model.stop();
    await new Promise((r) => setTimeout(r, 300));
    model = startScriptedModel({
      scripts: [
        { match: 'Jesteś agentem programistycznym',
          steps: [say('Ten plik konfiguruje budowanie. Nic nie zmieniałem.')] },
        ...enforcedPipelineScripts(),
      ],
      port,
    });
    await new Promise((r) => setTimeout(r, 800));

    await openSession(page, 'samo pytanie');
    const box = page.locator('#cs-session-view textarea').first();
    await box.waitFor({ state: 'visible', timeout: 25_000 });
    await box.fill('Co robi ten plik?');
    await box.press('Enter');

    await answerFor(page, 45_000);

    const roles = rolesConsulted();
    expect(roles.has('orkiestrator'), 'orkiestrator nie dostal pytania').toBe(true);
    for (const role of ['planista', 'wykonawca', 'tester']) {
      expect(roles.has(role), `tura bez zmian niepotrzebnie zwolala role "${role}"`).toBe(false);
    }
  });
});
