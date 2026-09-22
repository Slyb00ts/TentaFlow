// =============================================================================
// File: tests/e2e/agent-accounts-code-studio.spec.js
// Description: C01 inside Code Studio, on a real node, with nothing stubbed
//              below the browser.
//
//              What this exercises: a session is pinned to the harness flow
//              (`resolve_harness_flow`), so the turn runs the graph for real —
//              the orchestrator, the plan loop, then the build loop, whose
//              implementer is `code-implementer` bound to a `mode="user"`
//              Claude Code account. That delegation cannot resolve an account
//              for the person running the session, and the run PARKS on the
//              account card instead of failing. The card is read from the DOM
//              the console really paints, the server's refusal of a permission
//              grant for it is read from the wire, and the deny is clicked in
//              the browser — after which the parked run settles as `failed`
//              with the C01 refusal as its note, which is read back from both
//              the workspace database and the run list.
//
//              WHY C01 IS REACHABLE OFFLINE: the ask needs NO account to
//              exist. `mode="user"` with no personal account of that engine is
//              `AccountRefusal::NoAccountForUser`, raised before any node,
//              engine or bridge is consulted — so the whole card, the refusal
//              of a grant and the settle-on-deny run end to end on a machine
//              that never signs in to a vendor.
//
//              THE OTHER HALF OF C01, and C02's positive chip, on a node with
//              no vendor CLI: `helpers/stand-in-cli.js` places a compiled
//              program at the exact path the node's own managed-CLI
//              installation check reads (`<cache>/coding-agents/codex/<version>/
//              {installation-complete,bin/codex}`) and records the runtime-node
//              rows the node matrix keeps. The node reaches it the way it
//              reaches the vendor's binary — through the PATH Core hands the
//              bridge — so the sign-in, the credential adoption, the wake-up of
//              the parked run and the CLI turn that follows all run the
//              product's own code. Nothing in the product has a test branch.
//
//              TWO QUESTIONS, NOT ONE, and the second is the design's: the
//              account card is raised while the run is being RESOLVED, and the
//              `cli_delegate` card only after that resolution succeeded and the
//              run row exists. `delegate_cli` sends `cli_delegate` past the PEP
//              in both delegation modes (step 5 of its own order), and with no
//              standing grant for the engine the PEP's answer is `AskUser`
//              (`pep.rs` rule 10). So the sign-in ends the account question and
//              the resumed run then asks its own — a credential is not a
//              permission — and the CLI instance is opened only once a person
//              answers that one. The resume is asserted BEFORE that answer, on
//              the run row's existence, so the two questions are never confused
//              for one another.
//
//              A THIRD STATE OF THE SAME ACCOUNT, in the last test: gone. The
//              account is signed in again, and then thrown away through the
//              screen a person owns ("Moje konta" → "Odłącz"), after which the
//              filesystem is asked about BOTH files the bridge ever held for it
//              — the canonical credential and the private login home — and the
//              next turn is asked to run. It refuses.
//
//              WHAT THIS FILE DOES NOT COVER, stated here so a green run is
//              not read as more than it is:
//
//                • A real sign-in at a real provider, a token the provider
//                  would accept, a token ROTATION, and any provider-side plan
//                  or subject. The stand-in writes a well-formed credential
//                  file, exits 0 and prints "Logged in" for `login status`,
//                  which is the shape the product reads; what the provider
//                  would have said about that credential is not tested here and
//                  cannot be without the vendor's own CLI and an account.
//                • `cli_model`'s OPTION LIST (G01's model select for a CLI
//                  agent). Its only option source is `services_repo::models`
//                  rows prefixed `<engine_id>/`, and the only writer is
//                  `coding_agent::sync_models`, reached through the supervisor
//                  once the bridge reports `auth.status.authenticated = true`.
//                  The stand-in DOES answer `model/list`, so the path is
//                  reachable here — but this file does not assert the resulting
//                  catalogue, because what a real Codex reports is not what the
//                  stand-in reports. The half that IS asserted elsewhere — the
//                  select keeping the stored model while the catalogue is empty
//                  — is covered in the G01 test of
//                  tests/e2e/agent-accounts.spec.js, which also carries the
//                  note; the same note is in
//                  docs/agent-accounts-technical-design.md, "E2E coverage of
//                  C01, C02 and cli_model".
// =============================================================================

const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');
const {
  startBinary, stopBinary, waitForServer, binaryExists, baseUrl,
} = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { installAddonInstance } = require('./helpers/addon-setup');
const {
  startScriptedModel, tool, say,
} = require('./helpers/scripted-model');
const { installStandInCli, credentialMatchesCli } = require('./helpers/stand-in-cli');

const PORT = 18312;
const WORK_DIR = path.join(__dirname, '../../.runtime/e2e-aa-code-studio');
const DB = path.join(WORK_DIR, 'accounts.db');
const HOME = path.join(WORK_DIR, 'home');
const WWW_DIR = path.join(__dirname, '../../tentaflow-core/www');

const WORKSPACE = `c01-${Date.now().toString(36)}`;
const TASK = 'Zleć subagentowi przygotowanie notatek o zmianie.';
const TURN_TIMEOUT = 120_000;

/// The Polish dictionary the console itself reads (`prepare()` below pins the
/// UI to `pl`), loaded once so the shared-account describe can assert on the
/// FILLED strings the product ships rather than a copy typed into this file —
/// a copy would still pass if the real key drifted.
const PL_I18N = JSON.parse(fs.readFileSync(path.join(WWW_DIR, 'i18n', 'pl.json'), 'utf8'));

function i18n(key, vars) {
  const text = key.split('.').reduce((node, part) => (node == null ? node : node[part]), PL_I18N);
  if (typeof text !== 'string') throw new Error(`missing pl.json key: ${key}`);
  return text.replace(/\{(\w+)\}/g, (whole, name) => (
    vars && name in vars ? String(vars[name]) : whole
  ));
}

/// The second half of this file, on the same node: the CLI the stand-in
/// occupies is the engine of the agent bound here, so the parked run has
/// something to resume ONCE an account exists.
const WORKSPACE_CLI = `c02-${Date.now().toString(36)}`;
const CLI_ENGINE = 'codex';
const CLI_MODEL = 'gpt-5-codex';
/// The name the operator gives the account, and the name the chip must carry:
/// the chip is a claim about WHICH account a CLI ran on, so the two are read as
/// one fact, not as a title and a string that happen to match.
const ACCOUNT_NAME = 'Codex E2E';
/// The temporary root the macOS supervisor holds for a child's whole lifetime
/// (`macos_supervisor::new_root`: `/private/tmp` + `tfp-` + 12 random bytes in
/// hex). The bridge refuses a root that does not have this shape, and its
/// presence on a process record is the difference between "the product ran a
/// CLI" and "the product ran a CLI under the sandbox".
const SUPERVISOR_ROOT = /^\/private\/tmp\/tfp-[0-9a-f]{24}$/;

/// The third half of C01, in the third describe below: the same missing-
/// credential refusal, but the agent's binding is `mode="global"` rather than
/// `mode="user"` — an administrator can fix it in place, a plain member
/// cannot, and the two must see DIFFERENT cards for the very same account.
/// `services/agent_account.rs::global_account` needs an organisation on the
/// principal before it will even look the grant up; until today's fix
/// `agents/run_manager.rs` passed `org_id: None` into every delegated run, so
/// that lookup refused EVERY global account as `NotGranted` before the
/// credential was ever read — the run failed at once with
/// `account_grant_denied` and no card ever appeared. The two turn tests below
/// are what would catch a regression of that fix: `connect`/`cancel` never
/// becoming visible (the whole card is gone), and the settled run's own note
/// carrying `account_grant_denied` instead of the C01 refusal.
const WORKSPACE_GLOBAL = `c01-global-${Date.now().toString(36)}`;
const SHARED_ACCOUNT_NAME = 'Claude Code — wspólne E2E';
const GLOBAL_MEMBER_USERNAME = 'globalny-e2e';
const GLOBAL_MEMBER_DISPLAY = 'Grzegorz Globalny';
const GLOBAL_MEMBER_INITIAL_PASSWORD = 'grzegorz12345';
const GLOBAL_MEMBER_PASSWORD = 'grzegorz-e2e-2026';

let server = null;
let model = null;
let workspaceId = '';
let sessionId = '';
let implementerAgentId = '';
/// The stand-in CLI installed in the second describe's setup. Module scope
/// because three tests share it — the tests run serially on one node.
let stand = null;
let cliWorkspaceId = '';
let cliSessionId = '';
let cliImplementerId = '';
let sharedAccountId = '';
let globalWorkspaceId = '';
let globalImplementerId = '';
let globalMemberUserId = '';
let adminGlobalSessionId = '';
let memberGlobalSessionId = '';

test.describe.configure({ mode: 'serial' });

/// The orchestrator's script. There is deliberately NO route for the CLI child:
/// `code-implementer` is bound to a CLI runtime, so its turn goes to
/// `delegate_cli` and never reaches a language model at all — the refusal that
/// is the subject of this suite happens before any engine is called.
///
/// A session is pinned to the harness flow (`resolve_harness_flow`), so the turn
/// does not merely run the orchestrator: the graph spawns a planner and a critic
/// in the plan loop, then the implementer and the tester in the build loop. The
/// CLI agent that parks on the account card is the graph's own implementer
/// node — this script never calls `core.agent_spawn` itself.
///
/// Every route is keyed on prose only that agent's system prompt contains. The
/// tool catalogue is embedded in every prompt, so a marker that is also a tool
/// name would route the wrong agent's request here.
///
/// Every route CYCLES, because this file drives two turns on one node — one per
/// describe, each in its own workspace — and a cursor that ran out would leave
/// the second turn with a plain answer. That matters most for the orchestrator:
/// the seeded flow gates the whole pipeline on `vars.tool_calls_total > 0`
/// (`db/seed.rs`, node `d1`), so an orchestrator that only talks never reaches
/// the implementer, and both C01 and C02 live behind that gate.
function c01Scripts() {
  return [
    {
      // The orchestrator only has to CALL a tool: the graph gates the whole
      // review pipeline on `tool_calls_total > 0`, and the pipeline is where the
      // delegated CLI agent lives.
      match: 'Jesteś agentem programistycznym',
      cycle: true,
      steps: [
        tool('core.workspace_info', {}),
        say('Zlecę przygotowanie notatek wykonawcy.'),
      ],
    },
    {
      // Deliberately NO `core.task_plan`: `task_gate` vetoes the loop's exit only
      // while `session_tasks` holds something not `done`, so a turn that wrote no
      // plan runs the build loop exactly ONCE. With a plan the implementer could
      // never close — it is the run that parks on the card — the loop would turn
      // its full ten rounds and raise ten cards, and a suite about C01 would
      // really be a suite about the loop budget.
      match: 'Jesteś planistą zmian w kodzie',
      cycle: true,
      steps: [say('Plan: przygotować notatki o zmianie w NOTES.md.')],
    },
    {
      // The plan loop's critic and the build loop's critic are the same agent,
      // and a review loop asks every round, so this answer must stay available.
      // The marker is the loop's exit condition (`CRITIC_APPROVED_MARKER`).
      match: 'Jestes krytykiem',
      repeat: true,
      steps: [say('Sprawdziłem względem pierwotnych wytycznych — BEZ UWAG.')],
    },
    {
      match: 'Uruchamiasz testy i buildy',
      cycle: true,
      steps: [say('Nie ma czego uruchamiać — wykonawca nie zaczął pracy.')],
    },
  ];
}

test.beforeAll(async () => {
  test.skip(!binaryExists(), 'tentaflow binary not built (target_shared/{release-fast,release,debug})');
  fs.rmSync(WORK_DIR, { recursive: true, force: true });
  fs.mkdirSync(WORK_DIR, { recursive: true });
  model = startScriptedModel({ scripts: c01Scripts() });
  server = startBinary({ port: PORT, db: DB, home: HOME, env: { TENTAFLOW_WWW_DIR: WWW_DIR } });
  await waitForServer(PORT, 60000);
});

test.afterAll(async () => {
  model?.stop();
  if (!server) return;
  const exited = new Promise((resolve) => server.once('exit', resolve));
  stopBinary(server);
  await Promise.race([exited, new Promise((r) => setTimeout(r, 10000))]);
  server = null;
});

// =============================================================================
// The node's own state
// =============================================================================

// A reader alongside the running instance. Code Studio keeps its runtime state
// in the per-workspace database (`<data>/code-studio/<workspace>/workspace.db`,
// `code_studio/paths.rs`), not in the core database, so the run rows the
// assertions below are about are read from there.
function sql(query, db = DB) {
  const out = execFileSync('/usr/bin/sqlite3', ['-json', '-cmd', '.timeout 5000', db, query], {
    encoding: 'utf8',
  }).trim();
  return out ? JSON.parse(out) : [];
}

function quote(value) {
  return `'${String(value).replace(/'/g, "''")}'`;
}

/// Every account bridge process on this machine.
///
/// The node starts one per account on demand (`agent_runtime::start_bridge`)
/// from the executable it stages under `<cache>/coding-agents/bridge/<engine>/
/// <source-hash>/server`, and invokes it by that FULL path — so the marker is
/// the directory, not a program name two other things could share.
function bridgeProcesses() {
  const out = execFileSync('/bin/ps', ['-axo', 'pid=,command='], { encoding: 'utf8' });
  return out.split('\n').filter((line) => line.includes('/coding-agents/bridge/'));
}

function dbAgentRuntime(name) {
  return sql(`SELECT runtime_json FROM agents WHERE name = ${quote(name)}`)[0]?.runtime_json ?? null;
}

/// One workspace's own runtime database.
function wsDb(workspace) {
  return path.join(HOME, 'data', 'code-studio', workspace, 'workspace.db');
}

function dbSessionRuns(workspace = workspaceId) {
  if (!workspace) return [];
  return sql(
    'SELECT run_id, ordinal, kind, trigger, parent_run_id, agent_id, status, model FROM session_runs ORDER BY ordinal',
    wsDb(workspace),
  );
}

function dbApprovals(workspace = workspaceId) {
  if (!workspace) return [];
  return sql(
    'SELECT capability, status, decision, target_pattern, decided_by, account_json FROM approvals',
    wsDb(workspace),
  );
}

/// The CLI instances of a workspace — the durable record of which account a
/// CLI turn ran on (C02). Nothing in the product deletes one, so a row that
/// survived the turn is what the chip was built from.
function dbCliInstances(workspace = workspaceId) {
  if (!workspace) return [];
  return sql(
    'SELECT id, session_id, run_id, engine_id, account_id, vendor_session_id, status FROM cli_instances',
    wsDb(workspace),
  );
}

async function api(page, action, payload) {
  return page.evaluate(async ([a, p]) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return p === null ? ApiBinary.one(a) : ApiBinary.action(a, p);
  }, [action, payload === undefined ? null : payload]);
}

const scope = (workspace = workspaceId, session = sessionId) => ({
  workspaceId: workspace, sessionId: session,
});

/// Applied before the app boots, for the same two reasons as the sibling
/// suite: `TENTAFLOW_WWW_DIR` hashes the frontend per request, so a running
/// instance keeps announcing a "new version" modal that would swallow the
/// clicks below; and a fresh instance defaults to English, while every string
/// asserted here is the Polish copy the mockups are written in.
/// Puts the console back on its shell after the node was restarted on the same
/// database, without insisting on a login card.
///
/// The page's own storage still holds the JWT the sign-in above was given, and
/// the node kept the key that signed it, so the ordinary outcome of a reload is
/// the shell straight away — `loginAsAdmin` would sit out its timeout waiting
/// for a card that never mounts. The card is still handled when it does appear,
/// because a token surviving a restart is the product's behaviour and not
/// something this test gets to assume.
async function reloadConsole(page) {
  await page.goto(`${baseUrl(PORT)}/`);
  await Promise.race([
    page.waitForSelector('#login-username input', { timeout: 30_000 }).catch(() => null),
    page.waitForSelector('aside, nav, [data-screen], #main, #app-shell', { timeout: 30_000 }).catch(() => null),
  ]);
  if (await page.locator('#login-username input').count() === 0) return;
  await loginAsAdmin(page, { port: PORT });
}

async function prepare(page) {
  await page.addInitScript(() => {
    localStorage.setItem('tentaflow_lang', 'pl');
    const kill = () => document.querySelectorAll('.update-overlay').forEach((el) => el.remove());
    document.addEventListener('DOMContentLoaded', () => {
      kill();
      new MutationObserver(kill).observe(document.documentElement, { childList: true, subtree: true });
    });
  });
}

/// Every question a session has open, as the wire reports them.
///
/// A session can hold more than one at a time, and they are about different
/// things: the account card is raised while the run is being resolved, and the
/// `cli_delegate` card only after that resolution succeeded. A reader that ever
/// saw one of them would not be able to say which card is on screen.
async function pendingApprovals(page, workspace = workspaceId, session = sessionId) {
  const body = await api(page, 'codeStudioApprovalsListRequest', {
    ...scope(workspace, session), status: 'pending',
  });
  return body?.approvals ?? [];
}

/// The account cards a session has open.
function pendingAccounts(page, workspace = workspaceId, session = sessionId) {
  return pendingApprovals(page, workspace, session)
    .then((rows) => rows.filter((a) => a.capability === 'account_login'));
}

// -----------------------------------------------------------------------------
// Node setup, shared by both describes
//
// Both run on ONE node and ONE database (the file-level `beforeAll` above), so
// everything here is idempotent: the second describe must be able to run after
// the first without redeploying a service or reinstalling an app.
// -----------------------------------------------------------------------------

/// Installs Code Studio, or enables the instance that is already there.
///
/// A native APP behind the app gate: boot reconciles its PACKAGE into the
/// catalog but never installs an instance, so on a fresh node every one of its
/// request families answers `AppUnavailable` until somebody installs it — the
/// same install the Addons screen performs.
async function installCodeStudio(page) {
  const existing = await page.evaluate(async () => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const rows = await ApiBinary.list('addonsListRequest', { arrayKey: 'addons' });
    return (rows ?? []).find((a) => (a.packageId ?? a.package_id) === 'code-studio') ?? null;
  });
  if (!existing) {
    await installAddonInstance(page, {
      packageId: 'code-studio', displayName: 'Code Studio', permissions: [],
    });
    return true;
  }
  // The value is written unconditionally rather than read off the row: the
  // handler treats the same value twice as an explicit no-op, so this cannot
  // restart a running app, while a spec reading `is_enabled` for itself would
  // be a second definition of what "installed but off" looks like. An instance
  // lands disabled, and the app gate refuses a disabled app.
  await api(page, 'addonToggleRequest', {
    addonId: existing.addonId ?? existing.addon_id, enabled: true,
  });
  return false;
}

/// Deploys the scripted model if this node has none, then returns the catalogue
/// name the agents are bound to. A provider for the ORCHESTRATOR only — the CLI
/// child never asks a language model.
async function provisionModel(page) {
  const listed = await api(page, 'serviceListRequest');
  const running = (listed?.services ?? []).some(
    (s) => (s.engineId ?? s.engine_id) === 'openai-compatible'
      && /running|degraded/.test(String(s.status ?? '')),
  );
  if (!running) {
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
  }

  let modelName = '';
  await expect.poll(async () => {
    const models = await api(page, 'modelListRequest');
    modelName = (models?.models ?? [])
      .map((m) => m.modelName ?? m.model_name)
      .find((n) => String(n).includes('harness-test')) ?? '';
    return modelName;
  }, { timeout: 90_000, message: 'the scripted model never reached the catalogue' })
    .toBeTruthy();
  return modelName;
}

/// Binds every `code-*` agent to `modelName` and the named agent to `runtime`,
/// as a full read-modify-upsert over the roster the node shipped.
///
/// Returns the agent id, which is how a run of that agent is picked out of the
/// session's run list — the graph spawns a planner and a critic too, so "the
/// implementer's run" is a filter by agent and not "the first child".
async function bindAgents(page, { modelName, runtime, name = 'code-implementer' }) {
  const listResp = await api(page, 'agentsListRequest', {});
  const agents = JSON.parse(listResp?.agentsJson ?? listResp?.agents_json ?? '[]');
  let agentId = '';
  for (const a of agents.filter((x) => String(x.name ?? '').startsWith('code-'))) {
    const d = await api(page, 'agentsDetailRequest', { agentId: a.id });
    const agent = JSON.parse(d?.agentJson ?? d?.agent_json ?? '{}');
    // `agents.model` is a NOT NULL-shaped field every agent carries and a fresh
    // install leaves empty.
    agent.model = modelName;
    if (agent.name === name) {
      agentId = a.id;
      agent.runtime = runtime;
    }
    await api(page, 'agentsUpsertRequest', { agentJson: JSON.stringify(agent) });
  }
  expect(agentId, `no ${name} in the seeded roster`).toBeTruthy();
  // The node really stored it — a screen that showed a CLI runtime over a plain
  // LLM agent would fail at the first turn for an unrelated reason.
  expect(JSON.parse(dbAgentRuntime(name))).toEqual(runtime);
  return agentId;
}

/// Lets the signed-in person create workspaces: creating one needs a per-user
/// grant, and the operator here is the admin.
async function grantWorkspaceCreator(page) {
  const me = await api(page, 'authMeRequest');
  const users = await api(page, 'usersListRequest');
  const mine = (users?.users ?? []).find((u) => (u.username ?? '') === (me?.username ?? 'admin'));
  await api(page, 'codeStudioWorkspaceCreatorGrantSetRequest', {
    userId: mine?.id ?? mine?.userId ?? mine?.user_id, granted: true,
  });
}

/// Enters Code Studio and stops on its list. Its tile is server-driven
/// (`data-target` = route id, `data-kind` = native) and NOT one of the
/// hardcoded `data-route` tiles in the module.
async function openStudio(page) {
  await page.goto(`${baseUrl(PORT)}/`);
  await page.locator('[data-view="apps-home"]').first().click();
  await page.locator('[data-target="code-studio"][data-kind="native"]').first().click();
  await page.waitForSelector('#cs-new, #cs-empty-new, #cs-table-host', { timeout: 30_000 });
}

/// Walks the wizard and returns the workspace id once the node reports it
/// `active`. The status is read from the listing rather than from the screen:
/// what the screen shows after a create is a projection of that row.
async function createWorkspace(page, name) {
  await page.locator('#cs-new, #cs-empty-new').first().click();
  await page.locator('#cs-wz-name input').first().fill(name);
  await page.locator('[data-action="next"]').first().click();
  await page.locator('[data-action="next"]').first().click();
  await page.locator('[data-action="next"]').first().click();

  let id = '';
  await expect.poll(async () => {
    const body = await api(page, 'codeStudioWorkspacesListRequest', {});
    const ws = (body?.workspaces ?? []).find((w) => w.name === name);
    if (ws) id = ws.id ?? ws.workspaceId ?? id;
    return ws?.status ?? 'missing';
  }, { timeout: 60_000 }).toBe('active');
  return id;
}

/// Opens a workspace. That opens its CHAT: the screen resumes the most recent
/// session, or creates one and enters it (`code-studio.js::goto` →
/// `openWorkspaceChat`). The session view is the destination of this click, not
/// a wizard step, so the session is read back from the listing.
async function openWorkspace(page, name) {
  await page.locator(`text=${name}`).first().click();
  await page.waitForSelector('#cs-session-view', { state: 'visible', timeout: 60_000 });
}

/// The session the console entered, read from the listing it will draw from.
async function currentSession(page, workspace) {
  let id = '';
  await expect.poll(async () => {
    const sessions = await api(page, 'codeStudioSessionsListRequest', { workspaceId: workspace });
    id = (sessions?.sessions ?? [])[0]?.id ?? (sessions?.sessions ?? [])[0]?.sessionId ?? '';
    return id;
  }, { timeout: 30_000, message: 'no session reached the listing' })
    .toBeTruthy();
  return id;
}

/// The supervisor root of the first process record of `kind`, waited for.
///
/// A record is DELETED when its child ends, so this reads the watcher's log and
/// not the directory as it stands — a sign-in ends in well under a second, and
/// a caller that only started looking afterwards would see nothing at all.
async function wrappedRoot(probe, kind, timeout = 120_000) {
  let root = '';
  await expect.poll(() => {
    root = probe.find(kind)?.supervisor_root ?? '';
    return root;
  }, {
    timeout,
    message: `no '${kind}' process record appeared; kinds seen: ${probe.kinds().join(', ') || 'none'}`,
  }).not.toBe('');
  return root;
}

// =============================================================================

test.describe('C01 w Code Studio — brak konta zatrzymuje delegację', () => {
  test('przygotowanie: model, agent CLI na koncie użytkownika, workspace i sesja', async ({ page }) => {
    test.setTimeout(240_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });
    await installCodeStudio(page);
    const modelName = await provisionModel(page);

    // The delegated agent runs on the CLI, on the account of WHOEVER runs the
    // session. `mode="user"` is what makes the refusal a question for the
    // person at the console instead of a broken organisation binding.
    implementerAgentId = await bindAgents(page, {
      modelName,
      runtime: { kind: 'cli', engine: 'claude-code', model: 'sonnet', account: { mode: 'user' } },
    });

    await grantWorkspaceCreator(page);
    await openStudio(page);
    workspaceId = await createWorkspace(page, WORKSPACE);
    await openWorkspace(page, WORKSPACE);
    sessionId = await currentSession(page, workspaceId);
  });

  test('brak konta parkuje przebieg na karcie, a odmowa kończy go powodem C01', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 180_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });
    // Same entry point as the first test: the workspace reopens on the session
    // the previous test created.
    await openStudio(page);
    await openWorkspace(page, WORKSPACE);

    const composer = page.locator('#cs-session-view textarea').first();
    await composer.fill(TASK);
    await composer.press('Enter');

    // ---------------------------------------------------------------------
    // 1. The card. It is built from the approvals poll alone, so waiting for
    //    it in the DOM also proves the row the server wrote is readable by the
    //    console — not merely that a request was made.
    // ---------------------------------------------------------------------
    const card = page.locator('#cs-session-view [data-answer]:not([hidden])');
    const connect = card.locator('[data-action="answer-login"]');
    const cancel = card.locator('[data-action="answer-deny"]');
    await connect.waitFor({ state: 'visible', timeout: TURN_TIMEOUT });

    // The two rows ARE the card: connect (key 1) and stop (key 2), with no
    // scope picker and no counter, because nothing is being granted.
    await expect(connect).toHaveAttribute('marker', '1');
    await expect(cancel).toHaveAttribute('marker', '2');
    await expect(card.locator('.cs-answer-head')).toContainText('Wymagane konto');
    await expect(connect).toHaveAttribute('label', /Połącz konto Claude Code/);
    await expect(cancel).toHaveAttribute('label', 'Anuluj to zadanie');
    // `who` names the agent, the application and the binding that was refused.
    await expect(card.locator('.cs-answer-head .who')).toContainText('code-implementer');
    await expect(card.locator('.cs-answer-head .who')).toContainText('Claude Code');
    await expect(card.locator('.cs-answer-head .who')).toContainText('konto użytkownika');
    // The account-less wording, not the relogin one: this person has no account
    // for the engine at all, so "sign in again" would send them at the wrong
    // remedy. Both come from the same key family and both are one string away.
    const question = await card.locator('.cs-answer-q').textContent();
    expect(question).toContain('Claude Code');
    expect(question).not.toContain('Zaloguj się ponownie');

    // ---------------------------------------------------------------------
    // 2. What the SERVER wrote. The card is a projection of this row, so the
    //    row is where the question has to be complete.
    // ---------------------------------------------------------------------
    await expect.poll(async () => (await pendingAccounts(page)).length, {
      timeout: 30_000,
      message: 'the console shows a card the node does not have pending',
    }).toBeGreaterThan(0);

    const open = await pendingAccounts(page);
    expect(open, 'expected exactly one open account card').toHaveLength(1);
    const row = open[0];
    expect(row.capability).toBe('account_login');
    expect(row.mandatory_interactive, 'the account card is not a permission').toBe(false);
    // Field by field: the wasm decoder publishes every key twice, snake_case
    // and camelCase, so `toEqual` on the object would compare the alias set too.
    expect(row.account, 'the card description did not travel on the row').toBeTruthy();
    expect(row.account.engine_id).toBe('claude-code');
    expect(row.account.engine_name).toBe('Claude Code');
    expect(row.account.mode).toBe('user');
    expect(row.account.agent_name).toBe('code-implementer');
    // No personal account exists, so there is no id and no name to offer —
    // that absence IS the difference between "connect one" and "sign in again".
    expect(row.account.account_id ?? null).toBeNull();
    expect(row.account.account_name ?? null).toBeNull();

    // ---------------------------------------------------------------------
    // 3. A grant is not an answer. `account_login` is answered by connecting
    //    the account, so every standing scope is refused at the write rather
    //    than silently ignored — and the refusal names the remedy.
    // ---------------------------------------------------------------------
    for (const decision of ['allow_once', 'allow_for_run', 'allow_for_session', 'always']) {
      const refused = await api(page, 'codeStudioApprovalDecideRequest', {
        ...scope(), approvalId: row.approval_id, decision,
      }).then(() => null, (err) => String(err?.message ?? err));
      expect(refused, `'${decision}' was accepted for an account card`).toBeTruthy();
      expect(refused).toContain('account_login');
      expect(refused).toContain('connecting the account');
      // Still open: a refused decision must not have settled the row. Counted
      // rather than hard-coded, so an extra question raised by something else
      // in the turn cannot make this pass or fail for the wrong reason.
      expect(await pendingAccounts(page), `'${decision}' settled the account card`)
        .toHaveLength(1);
    }

    // ---------------------------------------------------------------------
    // 4. The person stops the turn — the click a real operator makes.
    //    `answer-login` (row 1) starts a vendor sign-in on the node and is
    //    deliberately NOT clicked: this suite must not reach a provider, and
    //    that branch's browser flow is exactly what A02 covers elsewhere.
    // ---------------------------------------------------------------------
    await cancel.click();

    // The row settles, so the card goes away on the next poll.
    await expect.poll(async () => (await pendingAccounts(page)).length, {
      timeout: 60_000,
      message: 'the card stayed pending after the operator stopped it',
    }).toBe(0);
    await expect(connect).toHaveCount(0);

    // ---------------------------------------------------------------------
    // 5. The parked run settles as failed, and the run list carries the C01
    //    reason: the timeline owns it (§13.3), and a bare 'failed' would send
    //    the operator to a SQL client to find out why.
    //    The run is picked by AGENT, not as "the first child": the graph spawns
    //    a planner and a critic before it ever reaches the implementer.
    // ---------------------------------------------------------------------
    let child = null;
    await expect.poll(async () => {
      const body = await api(page, 'codeStudioSessionRunsRequest', scope());
      child = (body?.runs ?? []).find(
        (r) => (r.parentRunId ?? r.parent_run_id)
          && (r.agentId ?? r.agent_id) === implementerAgentId,
      );
      return child?.status ?? 'missing';
    }, { timeout: 90_000, message: 'the delegated run never settled after the deny' })
      .toBe('failed');
    expect(child.note, 'the failed run carries no reason').toBeTruthy();
    expect(child.note).toContain('could not be delegated (C01)');
    expect(String(child.note)).toContain('Claude Code');
    // C02's absence rule: a run with no `cli_instances` row names no account.
    // The delegation never started a CLI, so a chip here would be a fabrication.
    expect(child.account, 'a run that never started a CLI named an account')
      .toBeFalsy();

    // ---------------------------------------------------------------------
    // 6. The same facts in the node's own database — the run row, the refusal
    //    that ended it, and the approval row the click settled.
    // ---------------------------------------------------------------------
    await expect.poll(() => dbSessionRuns().length).toBeGreaterThan(1);
    const runs = dbSessionRuns();
    // Exactly ONE run for the CLI agent, and it is the delegated one. The graph
    // spawns other roles (planner, critic, tester) in the same session, so the
    // filter is by agent: "there is exactly one implementer run" is the fact,
    // "there is exactly one subagent row" would be a lie about the pipeline.
    const spawned = runs.filter((r) => r.parent_run_id && r.agent_id === implementerAgentId);
    expect(spawned, 'no delegated run row in the workspace database').toHaveLength(1);
    expect(spawned[0].kind).toBe('subagent');
    expect(spawned[0].trigger).toBe('agent_spawn');
    expect(spawned[0].status).toBe('failed');
    // The root run is the operator's own turn, and the CLI run `delegate_cli`
    // opens (kind='cli') was never created — that row is written only after the
    // account resolves, which is the whole point of the card.
    expect(runs.filter((r) => r.kind === 'cli')).toHaveLength(0);
    expect(runs.filter((r) => r.kind === 'root')).toHaveLength(1);

    const accounts = dbApprovals().filter((a) => a.capability === 'account_login');
    expect(accounts, 'no account question row in the workspace database').toHaveLength(1);
    expect(accounts[0].status).toBe('decided');
    expect(accounts[0].decision).toBe('deny');
    // The target is the engine: "this run has no account for claude-code" is the
    // question, and a row that named no target could not be read back.
    expect(accounts[0].target_pattern).toBe('claude-code');
    expect(accounts[0].decided_by, 'the decision is attributed to a person').toBeTruthy();
    expect(JSON.parse(accounts[0].account_json)).toEqual({
      engine_id: 'claude-code',
      engine_name: 'Claude Code',
      mode: 'user',
      agent_name: 'code-implementer',
      account_id: null,
      account_name: null,
    });
    // Nothing else was left hanging: a row still `pending` after the turn ended
    // is a question nobody can answer any more.
    expect(dbApprovals().filter((a) => a.status === 'pending')).toHaveLength(0);
  });
});

// =============================================================================
// The other half of C01, and C02's positive chip
//
// Same node, same database, same serial file. What changes is the agent's
// engine: `code-implementer` is bound to `codex`, and the stand-in CLI occupies
// the path the node's own managed-CLI check reads. Nothing below stubs the
// product — the sign-in, the credential adoption and the CLI turn all run the
// node's own code, on a program the node reaches exactly the way it reaches the
// vendor's (`services/agent_runtime.rs::start_bridge` puts the engine's `bin/`
// FIRST on the PATH the bridge runs with).
// =============================================================================

test.describe('C01 resumowane przez logowanie i chip konta C02', () => {
  test('przygotowanie: podstawiony CLI na nodzie, agent na codex, workspace i sesja', async ({ page }) => {
    test.setTimeout(240_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });
    await installCodeStudio(page);
    const modelName = await provisionModel(page);

    // The stand-in, on the SAME home the node runs on and recorded in the SAME
    // database it reads. `installStandInCli` fills two contracts the product
    // itself defines — the cache tree `<cache>/coding-agents/<engine>/<version>/
    // {installation-complete,bin/<exe>}` and the `agent_runtime_nodes` /
    // `agent_runtime_engines` rows — and adds nothing else.
    stand = installStandInCli({ db: DB, home: HOME });

    cliImplementerId = await bindAgents(page, {
      modelName,
      runtime: {
        kind: 'cli', engine: CLI_ENGINE, model: CLI_MODEL, account: { mode: 'user' },
      },
    });

    await grantWorkspaceCreator(page);
    await openStudio(page);
    cliWorkspaceId = await createWorkspace(page, WORKSPACE_CLI);
    await openWorkspace(page, WORKSPACE_CLI);
    cliSessionId = await currentSession(page, cliWorkspaceId);
  });

  test('logowanie u dostawcy wznawia wstrzymaną turę, delegację zatwierdza osoba, a CLI pracuje w sandboxie', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 300_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });
    await openStudio(page);
    await openWorkspace(page, WORKSPACE_CLI);

    const composer = page.locator('#cs-session-view textarea').first();
    await composer.fill(TASK);
    await composer.press('Enter');

    // ---------------------------------------------------------------------
    // 1. The card. Same shape as the sibling describe's, with this engine's
    //    words: `mode="user"` and no personal codex account is
    //    `AccountRefusal::NoAccountForUser`, raised before any node, engine or
    //    bridge is consulted.
    // ---------------------------------------------------------------------
    const card = page.locator('#cs-session-view [data-answer]:not([hidden])');
    const connect = card.locator('[data-action="answer-login"]');
    await connect.waitFor({ state: 'visible', timeout: TURN_TIMEOUT });

    await expect(connect).toHaveAttribute('marker', '1');
    await expect(card.locator('.cs-answer-head')).toContainText('Wymagane konto');
    await expect(connect).toHaveAttribute('label', /Połącz konto Codex/);
    await expect(card.locator('.cs-answer-head .who')).toContainText('code-implementer');
    await expect(card.locator('.cs-answer-head .who')).toContainText('Codex');
    await expect(card.locator('.cs-answer-head .who')).toContainText('konto użytkownika');
    const question = await card.locator('.cs-answer-q').textContent();
    expect(question).toContain('Codex');
    // The account-less wording, not the relogin one: this person has no codex
    // account at all, so "sign in again" would send them at the wrong remedy.
    expect(question).not.toContain('Zaloguj się ponownie');

    // The server wrote the row the card is a projection of, and it is still
    // OPEN: the whole point of C01 is that this one waits for a person.
    const open = await pendingAccounts(page, cliWorkspaceId, cliSessionId);
    expect(open, 'expected exactly one open account card').toHaveLength(1);
    expect(open[0].capability).toBe('account_login');
    expect(open[0].account.engine_id).toBe(CLI_ENGINE);
    expect(open[0].account.mode).toBe('user');
    expect(open[0].account.agent_name).toBe('code-implementer');
    expect(open[0].account.account_id ?? null).toBeNull();
    const approvalId = open[0].approval_id;

    // And the parked state itself: `delegate_cli` opens the CLI run row ONLY
    // after the account resolves, so its absence here is what the sign-in is
    // about to end. The sibling describe proves the same absence is the steady
    // state when the account never arrives.
    const parked = (await api(page, 'codeStudioSessionRunsRequest',
      scope(cliWorkspaceId, cliSessionId)))?.runs ?? [];
    expect(parked.filter((r) => r.kind === 'cli'),
      'a CLI run exists while the account card is still open').toHaveLength(0);

    // ---------------------------------------------------------------------
    // 2. The operator connects the account — the click a real person makes.
    //    It opens A01 (create, scope=user, the card's engine only) and then
    //    A02 on top of it.
    // ---------------------------------------------------------------------
    await connect.click();
    const createForm = page.locator('tf-window').filter({ has: page.locator('.aa-form') }).last();
    await createForm.locator('[data-field="name"] input').first().waitFor({
      state: 'visible', timeout: 30_000,
    });
    await createForm.locator('[data-field="name"] input').first().fill(ACCOUNT_NAME);
    // The kind the create window offers first for codex IS the sign-in
    // (`credentialKindsFor` pushes `provider_login` ahead of `api_key`), which
    // is what makes `onCreated` chain straight into A02.
    await createForm.locator('[data-act="create"]').first().click();

    const login = page.locator('tf-window').filter({ has: page.locator('.aa-login') }).last();
    await login.locator('[data-act="start"]').first().waitFor({ state: 'visible', timeout: 30_000 });

    // The account exists on the node now, and its id is what names the account
    // root the credential and the process records live under. Read from the row
    // the server wrote, so the two are the same account by construction.
    let accountId = '';
    await expect.poll(() => {
      accountId = stand.accountIdForUser();
      return accountId;
    }, { timeout: 30_000, message: 'the created account never reached the node' }).toBeTruthy();

    // From here on the watcher collects every process record the account
    // writes. A record is DELETED when its child ends and a sign-in ends in
    // well under a second, so a poller started after the fact would see
    // nothing: the interval is short for the same reason.
    const probe = stand.watchProcessRecords(accountId, { intervalMs: 5 });

    await login.locator('[data-act="start"]').first().click();
    // The URL is read from the DOM the console really paints: the node opened a
    // terminal on the CLI, read it, and what the program printed is on screen.
    const url = login.locator('a[data-url]');
    await expect(url).toHaveAttribute(
      'href', 'https://stand-in.invalid/device?code=STAND-IN-CODE', { timeout: 90_000 },
    );
    await expect(login.locator('[data-field="code"] input'))
      .toBeEnabled({ timeout: 90_000 });

    // PROOF THIS IS NOT A BYPASS. Two independent facts: the sign-in really ran
    // as a child of the bridge (the record exists and names the account), and it
    // really ran inside the platform sandbox — `supervisor_root` is the
    // temporary root the macOS supervisor holds for the child's lifetime, and
    // the bridge refuses a root that is not `/private/tmp/tfp-<24 hex>`.
    const loginRoot = await wrappedRoot(probe, 'cli-login');
    expect(loginRoot, `process kinds seen: ${probe.kinds().join(', ')}`)
      .toMatch(SUPERVISOR_ROOT);

    await login.locator('[data-field="code"] input').first().fill('STAND-IN-CODE');
    await login.locator('[data-act="submit"]').first().click();
    // The identity branch, not its prefix: `login.result_ok` and
    // `login.result_ok_as` both begin "Konto zostało zalogowane", so a prefix
    // assertion cannot tell a person which account was connected — the exact
    // text can. `account:stand-in-account` is the subject the CLI's own
    // credential names, and this fixture states no plan: the panel composes
    // subject and plan with `filter(Boolean)`, so an absent plan leaves no
    // separator behind.
    await expect(login.locator('[data-result]')).toHaveText(
      'Konto zostało zalogowane jako account:stand-in-account.', { timeout: 120_000 },
    );

    // Done with the window: closing it is what the person would do, and the
    // run must resume anyway — the wake-up is the node's, not the console's.
    await page.keyboard.press('Escape');
    await expect(login).toHaveCount(0, { timeout: 30_000 });

    // ---------------------------------------------------------------------
    // 3. The parked run RESUMES, with NO click on the account card. Nothing
    //    below touches that card: the wake-up is the sign-in's, and the
    //    observable is the run `delegate_cli` opens — a row written only after
    //    the account resolved, so its appearance is the resume itself and not
    //    another question.
    // ---------------------------------------------------------------------
    const sessions = scope(cliWorkspaceId, cliSessionId);
    let cliRun = null;
    await expect.poll(async () => {
      const body = await api(page, 'codeStudioSessionRunsRequest', sessions);
      cliRun = (body?.runs ?? []).find((r) => r.kind === 'cli') ?? null;
      return cliRun?.status ?? 'missing';
    }, { timeout: 180_000, message: 'the parked run never resumed after the sign-in' })
      .not.toBe('missing');

    // ---------------------------------------------------------------------
    // 3b. The resumed run asks its OWN question, and it is a different one.
    //     `cli_delegate` goes past the PEP in both delegation modes (step 5 of
    //     the node's own order): with no standing grant for this engine the
    //     answer is `AskUser`, so the operator is asked whether this run may
    //     delegate a turn at all. The sign-in settled the account and nothing
    //     else — a credential is not a permission — so the run waits here, and
    //     the CLI instance below exists only once a person answers.
    // ---------------------------------------------------------------------
    const delegationCard = page.locator('#cs-session-view [data-answer]:not([hidden])');
    const allowOnce = delegationCard
      .locator('[data-action="answer-scope"][data-scope="allow_once"]');
    await allowOnce.waitFor({ state: 'visible', timeout: 120_000 });

    const asked = (await pendingApprovals(page, cliWorkspaceId, cliSessionId))
      .filter((a) => a.capability === 'cli_delegate');
    expect(asked, 'the delegation question is not on the server').toHaveLength(1);
    // The words are the PEP's own (rule 10's summary), not the console's: the
    // card must say what is being asked before a person answers it.
    expect(asked[0].summary).toContain('cli_delegate');
    expect(asked[0].mandatory_interactive,
      'a permission the operator may switch off is not a mandatory question').toBe(false);
    expect(asked[0].account ?? null,
      'a permission carries no account description').toBeNull();
    // The durable row names the engine: "always allow delegating to codex" is a
    // permission somebody can read back, a grant with no target is not.
    const delegationRow = dbApprovals(cliWorkspaceId)
      .filter((a) => a.capability === 'cli_delegate');
    expect(delegationRow, 'the delegation question is not in the workspace database')
      .toHaveLength(1);
    expect(delegationRow[0].target_pattern).toBe(CLI_ENGINE);
    expect(delegationRow[0].status).toBe('pending');
    await allowOnce.click();

    // ---------------------------------------------------------------------
    // 3d. C02's chip on the surface the mockup draws it on: the session
    //     NOW-BAR (`mockups/agent-accounts-20260917/c02-sesja.html`), which is
    //     a different element from the dock row asserted in the sibling test.
    //     The same run, the same label the host composes for it.
    //
    //     The bar's content is fed by `loadRuns` in `code-studio-session.js`,
    //     not by the event that starts a run, so the label lands on the
    //     session's own refresh cadence — every `SIDE_POLL_TICKS`-th
    //     `TIMELINE_POLL_MS` tick (`code-studio-session.js:134-135`, about ten
    //     seconds) — and the widget clears and hides the bar in its own
    //     `_render` (`tf-agent-activity.js:507-514`) as soon as no run is live.
    //     The `.cs-nowbar` container is never hidden by JS and its `hidden`
    //     attribute in the template is inert: `code-studio.css:710-717` gives
    //     it `display:flex`, which outranks the user-agent `[hidden]` rule. The
    //     fixture's turn outlives that interval for this reason among the ones
    //     in `stand-in-cli.c`.
    // ---------------------------------------------------------------------
    const nowChip = page.locator(
      '#cs-session-view .cs-nowbar [data-activity="now"] .tf-aa-bar tf-chip',
    );
    await expect(nowChip).toHaveText(`Codex · konto użytkownika: ${ACCOUNT_NAME}`, {
      timeout: 60_000,
    });

    await expect.poll(async () => {
      const body = await api(page, 'codeStudioSessionRunsRequest', sessions);
      cliRun = (body?.runs ?? []).find((r) => r.kind === 'cli') ?? null;
      return cliRun?.status ?? 'missing';
    }, { timeout: TURN_TIMEOUT, message: 'the delegation question was answered and the CLI turn '
      + 'still never completed' })
      .toBe('completed');

    // The click landed on the durable row, and the scope is the narrow one: the
    // person allowed THIS run, so the question comes back for the next one.
    const settled = dbApprovals(cliWorkspaceId)
      .filter((a) => a.capability === 'cli_delegate');
    expect(settled).toHaveLength(1);
    expect(settled[0].status).toBe('decided');
    expect(settled[0].decision).toBe('allow_once');
    expect(settled[0].decided_by, 'the decision is attributed to a person').toBeTruthy();

    const body = await api(page, 'codeStudioSessionRunsRequest', sessions);
    const runs = body?.runs ?? [];
    const implementerRun = runs.find(
      (r) => (r.parentRunId ?? r.parent_run_id) && (r.agentId ?? r.agent_id) === cliImplementerId,
    );
    const cli = runs.find((r) => r.kind === 'cli');
    expect(implementerRun, 'the delegated run is not in the run list').toBeTruthy();
    expect(implementerRun.status, 'the delegating run did not complete').toBe('completed');
    // The CLI run is the CHILD of the delegated run: `delegate_cli` opens it
    // under the sub-agent it belongs to, so a run chain that shows it at the
    // top would be a different graph.
    expect(cli.parentRunId ?? cli.parent_run_id)
      .toBe(implementerRun.runId ?? implementerRun.run_id);
    // The model is written when the run SETTLES (`finish_run`), so a completed
    // CLI run is the only place the delegated model is a fact.
    expect(cli.model).toBe(CLI_MODEL);
    // No refusal note: the run was not the one that failed.
    expect(cli.note ?? null, 'a completed run carries no refusal note').toBeNull();

    // ---------------------------------------------------------------------
    // 4. The account card was answered BY THE SIGN-IN, and the row says so.
    //    `allow_once` is the whole decision: a sign-in proves the account works
    //    now, not that the next run may skip the question.
    // ---------------------------------------------------------------------
    await expect.poll(async () => {
      const cards = await pendingAccounts(page, cliWorkspaceId, cliSessionId);
      return cards.length;
    }, { timeout: 30_000, message: 'the account card stayed pending after the sign-in' })
      .toBe(0);
    await expect(connect).toHaveCount(0);

    const decisions = dbApprovals(cliWorkspaceId).filter(
      (a) => a.capability === 'account_login' && a.target_pattern === CLI_ENGINE,
    );
    expect(decisions, 'the sign-in did not settle the account row').toHaveLength(1);
    expect(decisions[0].decision).toBe('allow_once');
    expect(decisions[0].status).toBe('decided');
    expect(decisions[0].decided_by, 'the decision is attributed to a person').toBeTruthy();
    // The row was written BEFORE the account existed and is not rewritten, so
    // it still describes the question that was asked: "no personal codex
    // account" — not the account the sign-in then produced.
    expect(JSON.parse(decisions[0].account_json)).toEqual({
      engine_id: CLI_ENGINE,
      engine_name: 'Codex',
      mode: 'user',
      agent_name: 'code-implementer',
      account_id: null,
      account_name: null,
    });

    // ---------------------------------------------------------------------
    // 5. The same facts in the node's own database: the CLI instance that ran,
    //    the account it ran on, and the credential the sign-in left.
    // ---------------------------------------------------------------------
    const instances = dbCliInstances(cliWorkspaceId);
    expect(instances, 'no CLI instance row for the resumed run').toHaveLength(1);
    expect(instances[0].engine_id).toBe(CLI_ENGINE);
    expect(instances[0].account_id).toBe(accountId);
    expect(instances[0].run_id).toBe(cli.runId ?? cli.run_id);
    // A closed instance, and both terminal states mean the same thing here: the
    // bridge marks a session it ended `ended`, and one it found already gone
    // `reaped`. Asserting either instead of the pair would be a claim about
    // which sweep ran first, not about the CLI having stopped.
    expect(['ended', 'reaped']).toContain(instances[0].status);

    // The credential the sign-in adopted is the one the CLI wrote, byte for
    // byte — the two files are one fact, and the JS and C copies of it are
    // compared rather than trusted to stay in step.
    expect(credentialMatchesCli(
      path.join(stand.accountRoot(accountId), 'credentials', CLI_ENGINE, 'auth.json'),
    )).toBe(true);

    // And the turn itself ran as a sandboxed child of the bridge: the second
    // half of the proof, on the process that actually did the work.
    const serverRoot = await wrappedRoot(probe, 'codex-app-server');
    expect(serverRoot, `process kinds seen: ${probe.kinds().join(', ')}`)
      .toMatch(SUPERVISOR_ROOT);
    probe.stop();

    // The card id is carried for the record: it names the row the assertions
    // above read, so a failure names the question rather than a position.
    expect(approvalId).toBeTruthy();
  });

  test('C02: przebieg CLI na koncie użytkownika nosi chip z nazwą konta', async ({ page }) => {
    test.setTimeout(180_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });
    await openStudio(page);
    await openWorkspace(page, WORKSPACE_CLI);

    // ---------------------------------------------------------------------
    // 1. What the SERVER says the run ran on. `cli_instances` is the durable
    //    record; `agents.runtime_json` says only what SHOULD resolve now, and a
    //    chip that followed the binding would relabel finished work.
    // ---------------------------------------------------------------------
    const sessions = scope(cliWorkspaceId, cliSessionId);
    let cliRun = null;
    await expect.poll(async () => {
      const body = await api(page, 'codeStudioSessionRunsRequest', sessions);
      cliRun = (body?.runs ?? []).find((r) => r.kind === 'cli') ?? null;
      return cliRun ? 'seen' : '';
    }, { timeout: 60_000, message: 'the session has no CLI run to chip' }).toBe('seen');

    expect(cliRun.account, 'the CLI run named no account').toBeTruthy();
    expect(cliRun.account.engine_name).toBe('Codex');
    expect(cliRun.account.mode).toBe('user');
    expect(cliRun.account.account_name).toBe(ACCOUNT_NAME);
    expect(cliRun.account.account_id).toBe(stand.accountIdForUser());

    // The counterpart, so a chip everywhere would fail: a run with no CLI
    // instance names no account. The orchestrator runs on a language model.
    const root = (await api(page, 'codeStudioSessionRunsRequest', sessions))?.runs
      ?.find((r) => r.kind === 'root');
    expect(root, 'the session has no root run').toBeTruthy();
    expect(root.account ?? null, 'a run that never ran a CLI named an account').toBeNull();

    // ---------------------------------------------------------------------
    // 2. The same fact in the DOM the console really paints: the agents dock
    //    lists the run, and the chip on its row carries the engine, the scope
    //    word and the account name.
    // ---------------------------------------------------------------------
    const runId = cliRun.runId ?? cliRun.run_id;
    const row = page.locator(`#cs-session-view [data-activity="dock"] .tf-aa-run[data-run="${runId}"]`);
    await row.waitFor({ state: 'visible', timeout: 60_000 });
    await expect(row).toContainText(`${ACCOUNT_NAME}`);
    await expect(row).toContainText('Codex');
    await expect(row).toContainText('konto użytkownika');

    // The chip is one string, not three coincidences: the row's own chip must
    // read exactly what the host composes for this engine, mode and account.
    const chip = row.locator('tf-chip').filter({ hasText: ACCOUNT_NAME });
    await expect(chip).toHaveCount(1);
    await expect(chip).toHaveText(`Codex · konto użytkownika: ${ACCOUNT_NAME}`);

  });

  test('konto bez działającego poświadczenia: most odmawia, przebieg nie ma tury', async ({ page }) => {
    test.setTimeout(400_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });
    await openStudio(page);
    await openWorkspace(page, WORKSPACE_CLI);

    // ---------------------------------------------------------------------
    // 1. The state this test is about: a credential the node can read that
    //    authenticates nobody. The empty object is what a CLI leaves behind
    //    when it wrote a file and nothing else, and the product refuses that
    //    shape in BOTH of its own credential writers, so "{}" is exactly "no
    //    credential that works here" — the state of a revoked, an expired or a
    //    never-signed-in account on this node. The CLI is asked about it the
    //    way the product asks: `codex login status`, which says "Not logged in"
    //    and exits non-zero for anything that is not a non-empty JSON object.
    // ---------------------------------------------------------------------
    const accountId = stand.accountIdForUser();
    const credential = path.join(
      stand.accountRoot(accountId), 'credentials', CLI_ENGINE, 'auth.json',
    );
    expect(fs.existsSync(credential), 'the sign-in left no credential to break').toBe(true);
    fs.writeFileSync(credential, '{}\n');

    // ---------------------------------------------------------------------
    // 2. A turn is asked for and the delegation is APPROVED. The account
    //    resolves — Core's store still holds this account's credential, and
    //    this test breaks the node's copy, not the store — so the question the
    //    person answers is the delegation one, and the refusal below is the
    //    account's own node refusing, not a resolution that never got started.
    // ---------------------------------------------------------------------
    const sessions = scope(cliWorkspaceId, cliSessionId);
    const runsNow = async () => ((await api(page, 'codeStudioSessionRunsRequest', sessions))?.runs ?? []);
    const before = new Set(
      (await runsNow()).filter((r) => r.kind === 'cli').map((r) => r.runId ?? r.run_id),
    );

    const composer = page.locator('#cs-session-view textarea').first();
    await composer.fill(TASK);
    await composer.press('Enter');

    const card = page.locator('#cs-session-view [data-answer]:not([hidden])');
    const allowOnce = card.locator('[data-action="answer-scope"][data-scope="allow_once"]');
    await allowOnce.waitFor({ state: 'visible', timeout: TURN_TIMEOUT });
    await allowOnce.click();

    // The product's outcome for a credential its CLI rejects: the run FAILS,
    // and the reason it carries is the bridge's own — `session_expired` is what
    // an engine whose authentication probe exits non-zero is reported as, and
    // Core keeps that word rather than inventing a reason of its own.
    let cli = null;
    await expect.poll(async () => {
      const fresh = (await runsNow()).filter(
        (r) => r.kind === 'cli' && !before.has(r.runId ?? r.run_id),
      );
      cli = fresh[0] ?? null;
      return cli?.status ?? 'missing';
    }, { timeout: TURN_TIMEOUT, message: 'the refused delegation never reached a terminal state' })
      .toBe('failed');
    const cliRunId = cli.runId ?? cli.run_id;
    expect(cli.note, 'the refused run carries no reason').toBe(
      'agent bridge /sessions: session_expired',
    );

    // The SAME failure in the session's own log, which is what makes the card
    // above a projection of something durable. `cli_delegation_authorized` is
    // the load-bearing event: it is written once the PEP has granted the
    // delegation, so its presence proves the account resolved and the refusal
    // came from that account's node.
    let events = [];
    await expect.poll(async () => {
      const timeline = await api(page, 'codeStudioSessionTimelineRequest', { ...sessions, limit: 500 });
      events = (timeline?.events ?? []).filter((e) => e.run_id === cliRunId);
      return events.some((e) => e.kind === 'run_finished') ? 'closed' : '';
    }, { timeout: 60_000, message: 'the refused run has no closing event' }).toBe('closed');
    expect(events.map((e) => e.kind)).toContain('cli_delegation_authorized');
    const finished = JSON.parse(events.find((e) => e.kind === 'run_finished').payload_json);
    expect(finished.RunFinished.status).toBe('failed');
    expect(finished.RunFinished.error).toBe('agent bridge /sessions: session_expired');

    // No turn ran and no vendor session was ever opened. The instance row is
    // written before the bridge is asked, so an EMPTY `vendor_session_id` is
    // what "the CLI never started" is in the durable record: a row exists for
    // the attempt, and nothing about it names a thread on the vendor's side.
    const instance = dbCliInstances(cliWorkspaceId).find((i) => i.run_id === cliRunId);
    expect(instance, 'the refused delegation left no instance row').toBeTruthy();
    expect(instance.vendor_session_id).toBe('');
    expect(['ended', 'reaped']).toContain(instance.status);
    // The refusing node does not repair the account on its own: the file is
    // what this test wrote, byte for byte. An account with no working
    // credential stays one until a person signs in again.
    expect(fs.readFileSync(credential, 'utf8')).toBe('{}\n');

    // The failure is not swallowed by the agent that delegated: the run the
    // person is waiting on ends as failed too.
    await expect.poll(async () => {
      const runs = await runsNow();
      const parent = runs.find(
        (r) => (r.runId ?? r.run_id) === (cli.parentRunId ?? cli.parent_run_id),
      );
      return parent?.status ?? 'missing';
    }, { timeout: 60_000, message: 'the delegating run never settled' }).toBe('failed');

    // And the console paints it: the failed CLI run's own row in the agents
    // dock, still naming the account whose CLI did not run.
    const row = page.locator(
      `#cs-session-view [data-activity="dock"] .tf-aa-run[data-run="${cliRunId}"]`,
    );
    await row.waitFor({ state: 'visible', timeout: 60_000 });
    await expect(row).toContainText(ACCOUNT_NAME);
    await expect(row).toContainText('błąd');
  });

  test('odłączenie konta usuwa oba pliki poświadczenia, a następna tura odmawia', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 240_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });

    // The third state of the same account, after "no working credential"
    // (the test above writes `{}`) and after a working one: GONE. The purge has
    // two halves in two places the node owns, the store row and the files the
    // bridge holds, and this walks the one a person drives — "Odłącz" on their
    // own card in "Moje konta" (U01), confirmed in the dialog the screen raises.
    // It is the same `providerAccountDeleteRequest` the administrator's account
    // window sends, and the same `account_delete` on the node.
    //
    // WHY THIS TEST IS LAST IN THE FILE: it removes the account the tests above
    // need. Everything it asserts about is built here, from the account's own
    // re-sign-in onwards.
    const accountId = stand.accountIdForUser();
    expect(accountId, 'the sign-in test left no codex account for this person').toBeTruthy();
    const accountRoot = stand.accountRoot(accountId);
    const canonical = path.join(accountRoot, 'credentials', CLI_ENGINE, 'auth.json');
    /// The bridge's private copy, made for its own sign-in, probe and discovery
    /// runs (`credentials.rs::login_root`). Nothing in the product removes it
    /// when the account's canonical credential goes, which is what makes it the
    /// durable half of this test: a purge that takes only the canonical file
    /// leaves a working provider credential sitting in plaintext.
    const loginCopy = path.join(accountRoot, 'login', CLI_ENGINE, 'auth.json');

    // ---------------------------------------------------------------------
    // 1. A working credential, produced the way the product produces one: the
    //    person signs their own account in. The test above left this account
    //    holding `{}`, so the sign-in is what makes the file below a credential
    //    rather than an empty object.
    // ---------------------------------------------------------------------
    await page.locator('.sidebar .nav-item[data-view="my-accounts"]').click();
    await page.waitForSelector('#myacc-apps .myapp-card', { timeout: 30_000 });
    const card = page.locator('#myacc-apps .myapp-card[data-engine="codex"]');
    await expect(card).toContainText(ACCOUNT_NAME);

    await card.locator('[data-role="app-login"]').click();
    const login = page.locator('tf-window').filter({ has: page.locator('.aa-login') }).last();
    await login.locator('[data-act="start"]').first().waitFor({ state: 'visible', timeout: 30_000 });
    await login.locator('[data-act="start"]').first().click();
    await expect(login.locator('a[data-url]')).toHaveAttribute(
      'href', 'https://stand-in.invalid/device?code=STAND-IN-CODE', { timeout: 90_000 },
    );
    await login.locator('[data-field="code"] input').first().fill('STAND-IN-CODE');
    await login.locator('[data-act="submit"]').first().click();
    await expect(login.locator('[data-result]')).toHaveText(
      'Konto zostało zalogowane jako account:stand-in-account.', { timeout: 120_000 },
    );
    await page.keyboard.press('Escape');
    await expect(login).toHaveCount(0, { timeout: 30_000 });

    // ---------------------------------------------------------------------
    // 2. Both copies exist, byte-identical, and the test is about them: the
    //    canonical file every session on this node reads, and the login home
    //    the bridge wrote the sign-in's credential into. A purge that leaves
    //    either one behind leaves a plaintext provider credential on disk.
    // ---------------------------------------------------------------------
    await expect.poll(() => credentialMatchesCli(canonical), {
      timeout: 60_000, message: 'the sign-in published no canonical credential',
    }).toBe(true);
    expect(credentialMatchesCli(loginCopy),
      'the sign-in left no credential in the bridge login home').toBe(true);

    // ---------------------------------------------------------------------
    // 3. The purge, through the GUI. The confirmation dialog is the screen's
    //    own, and the store half of the purge is read back from the node's
    //    database rather than from the toast.
    // ---------------------------------------------------------------------
    await card.locator('[data-role="app-disconnect"]').click();
    const confirm = page.locator('tf-window').filter({ hasText: 'Odłączyć konto?' }).last();
    await confirm.locator('tf-button[data-action="confirm"]').click();

    await expect.poll(() => stand.accountIdForUser(), {
      timeout: 60_000, message: 'the account row survived the purge',
    }).toBe('');
    expect(fs.existsSync(canonical), 'the canonical credential survived the purge').toBe(false);
    // THE ASSERTION THIS TEST EXISTS FOR. Before the fix the node's own route
    // removed the canonical root only, and its comment claimed otherwise.
    expect(fs.existsSync(loginCopy),
      'the bridge login home kept its copy of the purged credential').toBe(false);

    // ---------------------------------------------------------------------
    // 4. The next turn refuses instead of running. With no account of this
    //    person's own for the engine, the resolved run parks on the account
    //    card, and the CLI run row `delegate_cli` writes only AFTER the account
    //    resolves must not appear.
    // ---------------------------------------------------------------------
    await openStudio(page);
    await openWorkspace(page, WORKSPACE_CLI);

    const sessions = scope(cliWorkspaceId, cliSessionId);
    const runsNow = async () => ((await api(page, 'codeStudioSessionRunsRequest', sessions))?.runs ?? []);
    const before = new Set(
      (await runsNow()).filter((r) => r.kind === 'cli').map((r) => r.runId ?? r.run_id),
    );

    const composer = page.locator('#cs-session-view textarea').first();
    await composer.fill(TASK);
    await composer.press('Enter');

    const parkedCard = page.locator('#cs-session-view [data-answer]:not([hidden])');
    const connect = parkedCard.locator('[data-action="answer-login"]');
    await connect.waitFor({ state: 'visible', timeout: TURN_TIMEOUT });
    await expect(parkedCard.locator('.cs-answer-head')).toContainText('Wymagane konto');
    await expect(connect).toHaveAttribute('label', /Połącz konto Codex/);

    // The refusal is the account's ABSENCE, not a credential the node rejects:
    // the row is gone, so the question names no account to sign in again.
    const asked = await pendingAccounts(page, cliWorkspaceId, cliSessionId);
    expect(asked, 'the refused turn raised no account question').toHaveLength(1);
    expect(asked[0].account.account_id ?? null).toBeNull();
    expect(asked[0].account.mode).toBe('user');

    expect(
      (await runsNow()).filter((r) => r.kind === 'cli' && !before.has(r.runId ?? r.run_id)),
      'a CLI turn started on the account the purge removed',
    ).toHaveLength(0);
    // Nothing republished the credential while the turn waited on its question:
    // a copy that came back would be the defect this whole file is about.
    expect(fs.existsSync(canonical), 'the purged credential came back').toBe(false);

    // The person stops the turn, so the file leaves no run parked behind it —
    // the same click the C01 test ends on.
    await parkedCard.locator('[data-action="answer-deny"]').click();
    await expect.poll(async () => (await pendingAccounts(page, cliWorkspaceId, cliSessionId)).length, {
      timeout: 60_000, message: 'the account card stayed open after the operator stopped it',
    }).toBe(0);
  });

  test('odłączenie konta bez działającego mostu usuwa oba pliki poświadczenia', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 420_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });
    await openStudio(page);
    await openWorkspace(page, WORKSPACE_CLI);

    // ---------------------------------------------------------------------
    // 1. A working credential, produced the way the product produces one. The
    //    test above left this person with no account at all, so this is a fresh
    //    one: the parked run's card opens A01, the create window chains into A02
    //    and the person signs in — the same path the sign-in test walks.
    // ---------------------------------------------------------------------
    const composer = page.locator('#cs-session-view textarea').first();
    await composer.fill(TASK);
    await composer.press('Enter');

    const card = page.locator('#cs-session-view [data-answer]:not([hidden])');
    const connect = card.locator('[data-action="answer-login"]');
    await connect.waitFor({ state: 'visible', timeout: TURN_TIMEOUT });
    await connect.click();

    const createForm = page.locator('tf-window').filter({ has: page.locator('.aa-form') }).last();
    await createForm.locator('[data-field="name"] input').first().waitFor({
      state: 'visible', timeout: 30_000,
    });
    await createForm.locator('[data-field="name"] input').first().fill(ACCOUNT_NAME);
    await createForm.locator('[data-act="create"]').first().click();

    const login = page.locator('tf-window').filter({ has: page.locator('.aa-login') }).last();
    await login.locator('[data-act="start"]').first().waitFor({ state: 'visible', timeout: 30_000 });
    await login.locator('[data-act="start"]').first().click();
    await expect(login.locator('a[data-url]')).toHaveAttribute(
      'href', 'https://stand-in.invalid/device?code=STAND-IN-CODE', { timeout: 90_000 },
    );
    await login.locator('[data-field="code"] input').first().fill('STAND-IN-CODE');
    await login.locator('[data-act="submit"]').first().click();
    await expect(login.locator('[data-result]')).toHaveText(
      'Konto zostało zalogowane jako account:stand-in-account.', { timeout: 120_000 },
    );
    await page.keyboard.press('Escape');
    await expect(login).toHaveCount(0, { timeout: 30_000 });

    let accountId = '';
    await expect.poll(() => {
      accountId = stand.accountIdForUser();
      return accountId;
    }, { timeout: 60_000, message: 'the signed-in account never reached the node' }).toBeTruthy();
    const accountRoot = stand.accountRoot(accountId);
    const canonical = path.join(accountRoot, 'credentials', CLI_ENGINE, 'auth.json');
    const loginCopy = path.join(accountRoot, 'login', CLI_ENGINE, 'auth.json');
    await expect.poll(() => credentialMatchesCli(canonical), {
      timeout: 60_000, message: 'the sign-in published no canonical credential',
    }).toBe(true);
    expect(credentialMatchesCli(loginCopy),
      'the sign-in left no credential in the bridge login home').toBe(true);

    // The turn the sign-in resumed asks its OWN question, and it is answered NO
    // rather than left open: a pending delegation keeps polling its account, and
    // what this test needs is a node with nothing running on the account before
    // the restart below.
    const delegation = page.locator('#cs-session-view [data-answer]:not([hidden])');
    const deny = delegation.locator('[data-action="answer-deny"]');
    await deny.waitFor({ state: 'visible', timeout: TURN_TIMEOUT });
    await deny.click();
    await expect.poll(async () => (await pendingApprovals(page, cliWorkspaceId, cliSessionId)).length, {
      timeout: 60_000, message: 'a question stayed open before the restart',
    }).toBe(0);

    // ---------------------------------------------------------------------
    // 2. The node is stopped and started again on the SAME database and the
    //    SAME home. This is the state a purge usually meets and the one the
    //    running-bridge purge never sees: the account and both of its files
    //    are on disk, the process that wrote them is gone, and there is no
    //    idleness to wait out because there is no process to be idle.
    //
    //    The bridge is asserted to BE there first: the sign-in started one and
    //    `IDLE_GRACE` is fifteen minutes, so its death below is the restart's
    //    doing and the empty list after it is a measurement rather than a
    //    pattern that never matches anything.
    // ---------------------------------------------------------------------
    expect(bridgeProcesses().length, 'no bridge was running to be stopped').toBeGreaterThan(0);
    const exited = new Promise((resolve) => server.once('exit', resolve));
    stopBinary(server);
    await Promise.race([exited, new Promise((r) => setTimeout(r, 20_000))]);
    server = startBinary({
      port: PORT, db: DB, home: HOME, keepDb: true, env: { TENTAFLOW_WWW_DIR: WWW_DIR },
    });
    await waitForServer(PORT, 60_000);

    // The subject, stated before the purge: nothing about the restart removed
    // the files, and no bridge came back with the node.
    expect(bridgeProcesses(), 'the restarted node is running an account bridge').toHaveLength(0);
    expect(stand.accountIdForUser()).toBe(accountId);
    expect(credentialMatchesCli(canonical),
      'the credential did not survive the restart').toBe(true);
    expect(fs.existsSync(loginCopy), 'the login home did not survive the restart').toBe(true);

    // ---------------------------------------------------------------------
    // 3. The purge, through the same screen and the same control a person uses:
    //    "Moje konta" → "Odłącz" on the account's own card, confirmed in the
    //    dialog the screen raises. Nothing below calls the node's route.
    // ---------------------------------------------------------------------
    await reloadConsole(page);
    await page.locator('.sidebar .nav-item[data-view="my-accounts"]').click();
    await page.waitForSelector('#myacc-apps .myapp-card', { timeout: 30_000 });
    const accountCard = page.locator('#myacc-apps .myapp-card[data-engine="codex"]');
    await expect(accountCard).toContainText(ACCOUNT_NAME);

    await accountCard.locator('[data-role="app-disconnect"]').click();
    const confirm = page.locator('tf-window').filter({ hasText: 'Odłączyć konto?' }).last();
    await confirm.locator('tf-button[data-action="confirm"]').click();

    // ---------------------------------------------------------------------
    // 4. THE ASSERTION THIS TEST EXISTS FOR, and the store half beside it. A
    //    purge told the operator it succeeded while both plaintext files stayed
    //    on disk whenever no bridge happened to be running: `IDLE_GRACE` gives a
    //    bridge fifteen minutes and a restart gives it none at all, so the
    //    running-bridge case the test above covers is the rare one. Read from
    //    the filesystem, not from the toast the screen raises.
    // ---------------------------------------------------------------------
    await expect.poll(() => stand.accountIdForUser(), {
      timeout: 60_000, message: 'the account row survived the purge',
    }).toBe('');
    expect(fs.existsSync(canonical),
      'the canonical credential survived a purge with no bridge running').toBe(false);
    expect(fs.existsSync(loginCopy),
      'the login home kept its copy of the purged credential').toBe(false);
    // Core removes the two trees whole, so no engine's file is left behind in
    // either of them — the account's sessions are not in these trees.
    expect(fs.existsSync(path.join(accountRoot, 'credentials'))).toBe(false);
    expect(fs.existsSync(path.join(accountRoot, 'login'))).toBe(false);
    // The store half, read from the node's database rather than from the toast:
    // the account row and the two rows that hang off it are the cascade the
    // delete performs, and a purge that took the files but left them would be a
    // screen and a store that disagree.
    for (const table of [
      'provider_accounts', 'provider_account_credentials', 'provider_account_node_state',
    ]) {
      expect(
        sql(`SELECT count(*) AS n FROM ${table} WHERE account_id = ${quote(accountId)}`)[0]?.n ?? 0,
        `${table} still names the purged account`,
      ).toBe(0);
    }
    // The purge raises no question of its own and leaves none behind: the
    // delegation was answered, not abandoned.
    expect(await pendingApprovals(page, cliWorkspaceId, cliSessionId)).toHaveLength(0);
  });

  test('odłączenie konta z działającym mostem: most odmawia, plik zostaje, operator to widzi', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 480_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });
    await openStudio(page);
    await openWorkspace(page, WORKSPACE_CLI);

    // ---------------------------------------------------------------------
    // 1. A working credential, produced the way the product produces one. The
    //    test above purged this person's account, so this is a fresh one
    //    through the same path: the parked run's card opens A01, the create
    //    window chains into A02 and the person signs in.
    // ---------------------------------------------------------------------
    const composer = page.locator('#cs-session-view textarea').first();
    await composer.fill(TASK);
    await composer.press('Enter');

    const card = page.locator('#cs-session-view [data-answer]:not([hidden])');
    const connect = card.locator('[data-action="answer-login"]');
    await connect.waitFor({ state: 'visible', timeout: TURN_TIMEOUT });
    await connect.click();

    const createForm = page.locator('tf-window').filter({ has: page.locator('.aa-form') }).last();
    await createForm.locator('[data-field="name"] input').first().waitFor({
      state: 'visible', timeout: 30_000,
    });
    await createForm.locator('[data-field="name"] input').first().fill(ACCOUNT_NAME);
    await createForm.locator('[data-act="create"]').first().click();

    const login = page.locator('tf-window').filter({ has: page.locator('.aa-login') }).last();
    await login.locator('[data-act="start"]').first().waitFor({ state: 'visible', timeout: 30_000 });
    await login.locator('[data-act="start"]').first().click();
    await expect(login.locator('a[data-url]')).toHaveAttribute(
      'href', 'https://stand-in.invalid/device?code=STAND-IN-CODE', { timeout: 90_000 },
    );
    await login.locator('[data-field="code"] input').first().fill('STAND-IN-CODE');
    await login.locator('[data-act="submit"]').first().click();
    await expect(login.locator('[data-result]')).toHaveText(
      'Konto zostało zalogowane jako account:stand-in-account.', { timeout: 120_000 },
    );
    await page.keyboard.press('Escape');
    await expect(login).toHaveCount(0, { timeout: 30_000 });

    let accountId = '';
    await expect.poll(() => {
      accountId = stand.accountIdForUser();
      return accountId;
    }, { timeout: 60_000, message: 'the signed-in account never reached the node' }).toBeTruthy();
    const accountRoot = stand.accountRoot(accountId);
    const canonical = path.join(accountRoot, 'credentials', CLI_ENGINE, 'auth.json');
    const loginCopy = path.join(accountRoot, 'login', CLI_ENGINE, 'auth.json');
    await expect.poll(() => credentialMatchesCli(canonical), {
      timeout: 60_000, message: 'the sign-in published no canonical credential',
    }).toBe(true);
    // The second tree, stated before the purge: without it the assertions after
    // it would hold for a login home that was never there.
    expect(credentialMatchesCli(loginCopy),
      'the sign-in left no credential in the bridge login home').toBe(true);

    // The turn the sign-in resumed asks its own question and it is answered NO,
    // so nothing is left running on the account before the purge below.
    const delegation = page.locator('#cs-session-view [data-answer]:not([hidden])');
    const deny = delegation.locator('[data-action="answer-deny"]');
    await deny.waitFor({ state: 'visible', timeout: TURN_TIMEOUT });
    await deny.click();
    await expect.poll(async () => (await pendingApprovals(page, cliWorkspaceId, cliSessionId)).length, {
      timeout: 60_000, message: 'a question stayed open before the purge',
    }).toBe(0);

    // ---------------------------------------------------------------------
    // 2. The precondition, stated before the purge pins anything: a bridge is
    //    RUNNING for this account, so the node's `drop_account_credential`
    //    takes the bridge arm (`DELETE /account/credential`) rather than
    //    removing the files itself. The test would fail loudly rather than
    //    quietly test the other arm — with no bridge up, Core removes both
    //    trees and the assertions four paragraphs down cannot hold.
    // ---------------------------------------------------------------------
    expect(bridgeProcesses().length, 'no bridge was running to refuse the purge').toBeGreaterThan(0);

    // The fixture: write permission removed from ONE engine directory inside
    // the canonical tree. Unlinking an entry needs write on its PARENT, so the
    // bridge's walk still READS the directory and is refused at `unlink`
    // (EACCES) — the shape of the macOS `uchg` and the held-open-file cases,
    // which are harder to build. Nothing is planted in the login home, so the
    // bridge reaches its second tree and empties it: that asymmetry is what the
    // assertions below read.
    const engineDir = path.dirname(canonical);
    fs.chmodSync(engineDir, 0o500);

    // ---------------------------------------------------------------------
    // 3. The purge, through the same screen and the same control a person
    //    uses: "Moje konta" → "Odłącz" on the account's own card, confirmed in
    //    the dialog the screen raises. Nothing below calls the node's route.
    // ---------------------------------------------------------------------
    await page.locator('.sidebar .nav-item[data-view="my-accounts"]').click();
    await page.waitForSelector('#myacc-apps .myapp-card', { timeout: 30_000 });
    const accountCard = page.locator('#myacc-apps .myapp-card[data-engine="codex"]');
    await expect(accountCard).toContainText(ACCOUNT_NAME);

    await accountCard.locator('[data-role="app-disconnect"]').click();
    const confirm = page.locator('tf-window').filter({ hasText: 'Odłączyć konto?' }).last();
    await confirm.locator('tf-button[data-action="confirm"]').click();

    // WHAT THE OPERATOR SEES, and the whole point of the acknowledgement: the
    // account was deleted, but a plaintext credential is still on this node, so
    // the screen must NOT print its success message over it. The mode is
    // restored as soon as the purge has run — it is a lock for the fixture, not
    // a state any assertion below needs, and a failure here must not leave a
    // directory the suite's own cleanup cannot empty.
    try {
      // `agent_accounts.purge_incomplete` — read from pl.json rather than
      // pinned here a second time, so a deliberate copy change (as already
      // happened once: this used to be a shorter sentence) cannot drift the
      // two apart again.
      await expect(page.locator('.toast.toast-error').last()).toContainText(
        i18n('agent_accounts.purge_incomplete'), { timeout: 60_000 },
      );
    } finally {
      fs.chmodSync(engineDir, 0o700);
    }

    // The half that DID complete, read from the node's database rather than
    // from the screen: the row and the cascade under it are gone, which is
    // exactly why nothing can come back for the file below — no reconcile
    // reaches an account whose rows no longer name it, and no screen can ask.
    await expect.poll(() => stand.accountIdForUser(), {
      timeout: 60_000, message: 'the account row survived the purge',
    }).toBe('');
    expect(
      sql(`SELECT count(*) AS n FROM provider_account_credentials WHERE account_id = ${quote(accountId)}`)[0]?.n ?? 0,
      'the store row that named the credential survived the purge',
    ).toBe(0);

    // The path itself, on the only channel that carries one: `AccountOpAck` has
    // no field for it, so the node's warning is where the operator learns WHICH
    // of the two trees still holds the token. It is the bridge's own sentence,
    // embedded by `call_bridge`, which is what makes the two arms' reports
    // equivalent rather than merely both non-empty.
    expect(
      server.logTail.join(''),
      'the node never named the tree it could not empty',
    ).toContain(path.join(accountRoot, 'credentials'));

    // ---------------------------------------------------------------------
    // 4. THE ASSERTION THIS TEST EXISTS FOR: the second tree was attempted and
    //    went, while the first one still holds a working credential. Before the
    //    fix the bridge returned at `credentials/`, so BOTH files stayed; the
    //    failure was reported, but a file the operator was never told the path
    //    of is one they cannot find.
    // ---------------------------------------------------------------------
    expect(fs.existsSync(loginCopy),
      'the login home survived a failure in the other tree, and it holds the same credential').toBe(false);
    expect(fs.existsSync(path.join(accountRoot, 'login')),
      'the login home was left behind as a directory').toBe(false);
    expect(fs.existsSync(canonical),
      'the locked tree was emptied after all, so the fixture pinned nothing').toBe(true);
    expect(credentialMatchesCli(canonical),
      'the file the refusal left behind is not the credential the sign-in published').toBe(true);
  });

  /// The purge a RUNNING bridge refuses while a sign-in is IN FLIGHT.
  ///
  /// The bridge's own rule is that it will not remove the credential while a
  /// sign-in holds it: the sign-in would write it back the moment it finished,
  /// so a removal promised there is a removal the bridge cannot keep, and
  /// `DELETE /account/credential` answers `login_in_progress` instead. That
  /// refusal is a statement about the SIGN-IN, not about the node's disk, and
  /// the account row is already gone by the time Core asks: a purge that
  /// returned the refusal as its outcome left a plaintext provider credential on
  /// the disk with nothing left in the store to name it, so no reconcile and no
  /// screen could ever reach it again. Core owns the account root, so the files
  /// are removed from there once the bridge is out of the way.
  ///
  /// The reload in the middle is part of the fixture, not decoration: the login
  /// window is modal (a click on anything behind it is intercepted) and its own
  /// close hook cancels an unfinished sign-in, so the disconnect control is only
  /// reachable once the page has been replaced. The node's side of the sign-in is
  /// detached from the page by design (`provider_accounts/login.rs::drive`), so a
  /// reload leaves it running — which is asserted here rather than assumed.
  test('odłączenie konta w trakcie logowania usuwa oba pliki poświadczenia', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 480_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });

    // ---------------------------------------------------------------------
    // 1. An account with a working credential, produced the way the product
    //    produces one: "Moje konta" → connect → create → sign in. No workspace
    //    and no delegation is needed for a purge, so this walks the shortest
    //    real path to a signed-in account.
    // ---------------------------------------------------------------------
    await page.locator('.sidebar .nav-item[data-view="my-accounts"]').click();
    await page.waitForSelector('#myacc-apps .myapp-card', { timeout: 30_000 });
    const card = page.locator('#myacc-apps .myapp-card[data-engine="codex"]');
    await card.locator('[data-role="app-connect"]').click();

    const createForm = page.locator('tf-window').filter({ has: page.locator('.aa-form') }).last();
    await createForm.locator('[data-field="name"] input').first().waitFor({
      state: 'visible', timeout: 30_000,
    });
    await createForm.locator('[data-field="name"] input').first().fill(ACCOUNT_NAME);
    await createForm.locator('[data-act="create"]').first().click();

    const login = page.locator('tf-window').filter({ has: page.locator('.aa-login') }).last();
    await login.locator('[data-act="start"]').first().waitFor({ state: 'visible', timeout: 30_000 });
    await login.locator('[data-act="start"]').first().click();
    await expect(login.locator('a[data-url]')).toHaveAttribute(
      'href', 'https://stand-in.invalid/device?code=STAND-IN-CODE', { timeout: 90_000 },
    );
    await login.locator('[data-field="code"] input').first().fill('STAND-IN-CODE');
    await login.locator('[data-act="submit"]').first().click();
    await expect(login.locator('[data-result]')).toHaveText(
      'Konto zostało zalogowane jako account:stand-in-account.', { timeout: 120_000 },
    );
    await page.keyboard.press('Escape');
    await expect(login).toHaveCount(0, { timeout: 30_000 });

    let accountId = '';
    await expect.poll(() => {
      accountId = stand.accountIdForUser();
      return accountId;
    }, { timeout: 60_000, message: 'the signed-in account never reached the node' }).toBeTruthy();
    const accountRoot = stand.accountRoot(accountId);
    const canonical = path.join(accountRoot, 'credentials', CLI_ENGINE, 'auth.json');
    const loginCopy = path.join(accountRoot, 'login', CLI_ENGINE, 'auth.json');
    await expect.poll(() => credentialMatchesCli(canonical), {
      timeout: 60_000, message: 'the sign-in published no canonical credential',
    }).toBe(true);
    expect(credentialMatchesCli(loginCopy),
      'the sign-in left no credential in the bridge login home').toBe(true);

    // ---------------------------------------------------------------------
    // 2. A SECOND sign-in, left in flight. The first one is finished and its
    //    process record is gone, so the record watched for below can only be
    //    this one's — a stale record would pin the wrong pid.
    // ---------------------------------------------------------------------
    expect(
      stand.processRecords(accountId).filter((record) => record.kind === 'cli-login'),
      'the finished sign-in left its process record behind',
    ).toHaveLength(0);
    const records = stand.watchProcessRecords(accountId);

    await card.locator('[data-role="app-login"]').click();
    const second = page.locator('tf-window').filter({ has: page.locator('.aa-login') }).last();
    await second.locator('[data-act="start"]').first().click();
    await expect(second.locator('a[data-url]')).toHaveAttribute(
      'href', 'https://stand-in.invalid/device?code=STAND-IN-CODE', { timeout: 90_000 },
    );

    // The process the bridge started for it, named rather than assumed: its
    // record exists only while the child lives (`process.rs`), and the child is
    // blocked on its stdin because no code has been typed into it.
    await expect.poll(() => records.find('cli-login')?.pid ?? 0, {
      timeout: 60_000, message: 'the bridge started no sign-in process to leave in flight',
    }).toBeGreaterThan(0);
    const signInPid = Number(records.find('cli-login').pid);
    const signInAlive = () => {
      try { process.kill(signInPid, 0); return true; } catch { return false; }
    };
    expect(signInAlive(), 'the sign-in process was not running before the purge').toBe(true);

    // ---------------------------------------------------------------------
    // 3. The page is replaced, which is what makes the disconnect control
    //    reachable at all while that sign-in is open. The sign-in itself is the
    //    node's, not the page's, and both halves of that are asserted here: the
    //    child is still running, and the credential the first sign-in wrote is
    //    still on the disk.
    // ---------------------------------------------------------------------
    await reloadConsole(page);
    expect(records.find('cli-login'), 'the sign-in did not survive the reload').toBeTruthy();
    expect(signInAlive(), 'the sign-in process did not survive the reload').toBe(true);
    expect(fs.existsSync(loginCopy), 'the login home did not survive the reload').toBe(true);

    // ---------------------------------------------------------------------
    // 4. The purge, through the screen and the control a person uses:
    //    "Moje konta" → "Odłącz" on the account's own card, confirmed in the
    //    dialog the screen raises. Nothing below calls the node's route.
    // ---------------------------------------------------------------------
    await page.locator('.sidebar .nav-item[data-view="my-accounts"]').click();
    await page.waitForSelector('#myacc-apps .myapp-card', { timeout: 30_000 });
    const accountCard = page.locator('#myacc-apps .myapp-card[data-engine="codex"]');
    await expect(accountCard).toContainText(ACCOUNT_NAME);

    await accountCard.locator('[data-role="app-disconnect"]').click();
    const confirm = page.locator('tf-window').filter({ hasText: 'Odłączyć konto?' }).last();
    await confirm.locator('tf-button[data-action="confirm"]').click();

    // WHAT THE OPERATOR IS TOLD, and the assertion that replaces "the bridge
    // said no". `reportWriteOutcome` prints the error key OR the caller's
    // sentence for one action and never both, so a success sentence here is the
    // whole claim: no toast told the operator a credential had stayed behind.
    await expect(page.locator('.toast.toast-success').last()).toContainText('Konto odłączone', {
      timeout: 60_000,
    });

    // ---------------------------------------------------------------------
    // 5. Both trees, the store, and the process the purge met. Read from the
    //    filesystem and the node's database rather than from the toast.
    // ---------------------------------------------------------------------
    await expect.poll(() => stand.accountIdForUser(), {
      timeout: 60_000, message: 'the account row survived the purge',
    }).toBe('');
    expect(fs.existsSync(canonical),
      'the canonical credential survived a purge the bridge had refused').toBe(false);
    expect(fs.existsSync(loginCopy),
      'the login home kept the copy of the credential the sign-in wrote').toBe(false);
    expect(fs.existsSync(path.join(accountRoot, 'login')),
      'the login home was left behind as a directory').toBe(false);
    expect(fs.existsSync(path.join(accountRoot, 'credentials')),
      'the canonical tree was left behind as a directory').toBe(false);
    for (const table of [
      'provider_accounts', 'provider_account_credentials', 'provider_account_node_state',
    ]) {
      expect(
        sql(`SELECT count(*) AS n FROM ${table} WHERE account_id = ${quote(accountId)}`)[0]?.n ?? 0,
        `${table} still names the purged account`,
      ).toBe(0);
    }

    // The sign-in child the purge met. A record is DELETED by the bridge only
    // after it has verified the pid is gone (`process.rs`), so its absence is
    // the node's own answer that nothing was left which could write the trees
    // back; the pid observation beside it is reported, not asserted, because a
    // released pid can be reused by an unrelated process.
    await expect.poll(() => stand.processRecords(accountId).some((r) => r.pid === signInPid), {
      timeout: 60_000, message: 'the bridge left the sign-in process record behind',
    }).toBe(false);
    records.stop();
    test.info().annotations.push({
      type: 'sign-in process',
      description:
        `pid ${signInPid} was alive while the purge ran and its process record was removed by the ` +
        `bridge; after the purge the pid is ${signInAlive() ? 'still observable' : 'gone'}`,
    });
  });
});

// =============================================================================
// The SHARED (global) account variant of C01 — a card an administrator can
// resolve, and the same card with the sign-in row withheld from anyone else.
//
// Same node, same database, same serial file. `code-implementer` is rebound a
// third time, to a `mode="global"` account: one that exists, is granted to the
// whole organisation, and deliberately carries NO credential, so
// `resolve_run_account` reaches `AccountRefusal::CredentialMissing` — the SAME
// refusal `describe`s above reach through `mode="user"`, but reached here only
// past the grant check `global_account()` performs first. That grant check is
// exactly what needed the principal's `org_id`, and is what a regression of
// today's fix (`agents/run_manager.rs` carrying `principal.org_id` into the
// delegated run) would break again.
// =============================================================================

test.describe('C01 (konto wspólne) w Code Studio — administrator i zwykły użytkownik widzą różne karty', () => {
  test('przygotowanie: konto wspólne bez poświadczenia, agent, workspace, uprawniony użytkownik', async ({ page }) => {
    test.setTimeout(240_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });
    await installCodeStudio(page);
    const modelName = await provisionModel(page);

    // A plain member. `installCodeStudio` grants NO default permission
    // (`permissions: []`), so without an explicit per-user grant below a
    // non-admin gets `AppUnavailable` before ever reaching a workspace.
    await api(page, 'iamCreateUserRequest', {
      username: GLOBAL_MEMBER_USERNAME,
      password: GLOBAL_MEMBER_INITIAL_PASSWORD,
      displayName: GLOBAL_MEMBER_DISPLAY,
      email: 'globalny-e2e@e2e.local',
      role: 'user',
      groupIds: [],
    });
    const users = await api(page, 'usersListRequest');
    globalMemberUserId = (users?.users ?? []).find((u) => u.username === GLOBAL_MEMBER_USERNAME)?.id;
    expect(globalMemberUserId, 'the member user was not created').toBeTruthy();

    const addons = await api(page, 'addonsListRequest');
    const csAddon = (addons?.addons ?? []).find((a) => (a.packageId ?? a.package_id) === 'code-studio');
    const codeStudioAddonId = csAddon?.addonId ?? csAddon?.addon_id;
    expect(codeStudioAddonId, 'code-studio addon instance not found').toBeTruthy();
    await api(page, 'addonPermissionSetRequest', {
      addonId: codeStudioAddonId,
      subjectType: 'user',
      subjectId: globalMemberUserId,
      permissionId: 'code_studio.read',
      grantMode: 'allow',
    });

    // A GLOBAL account for Claude Code, granted to the WHOLE organisation (an
    // org-wide grant covers both the admin and the member below), and
    // deliberately left with no credential ever set — a sign-in is exactly
    // the one thing this test never does. `resolve_run_account` reaches
    // `CredentialMissing` only once the grant check has passed, so a card at
    // all — for either person — is already proof the grant resolved.
    const created = await api(page, 'providerAccountCreateRequest', {
      engineId: 'claude-code',
      displayName: SHARED_ACCOUNT_NAME,
      scope: 'global',
      credentialKind: 'provider_login',
    });
    sharedAccountId = created?.account?.accountId ?? created?.account?.account_id;
    expect(sharedAccountId, 'the shared account was not created').toBeTruthy();
    await api(page, 'providerAccountGrantsSetRequest', {
      accountId: sharedAccountId,
      grants: [{ subject_type: 'org', subject_id: '' }],
    });

    globalImplementerId = await bindAgents(page, {
      modelName,
      runtime: {
        kind: 'cli',
        engine: 'claude-code',
        model: 'sonnet',
        account: { mode: 'global', account_id: sharedAccountId },
      },
    });

    await grantWorkspaceCreator(page);
    await openStudio(page);
    globalWorkspaceId = await createWorkspace(page, WORKSPACE_GLOBAL);
    await openWorkspace(page, WORKSPACE_GLOBAL);
    adminGlobalSessionId = await currentSession(page, globalWorkspaceId);

    // The member needs WORKSPACE membership too: app-level read only shows
    // the studio, and a session lives inside one workspace — `session_open_v1`
    // needs at least Editor there to open one of their own.
    await api(page, 'codeStudioWorkspaceMemberSetRequest', {
      workspaceId: globalWorkspaceId, userId: globalMemberUserId, role: 'editor',
    });
  });

  test('administrator widzi kartę wspólnego konta z opcją logowania i kończy zadanie', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 180_000);
    await prepare(page);
    await loginAsAdmin(page, { port: PORT });
    await openStudio(page);
    await openWorkspace(page, WORKSPACE_GLOBAL);

    const composer = page.locator('#cs-session-view textarea').first();
    await composer.fill(TASK);
    await composer.press('Enter');

    // ---------------------------------------------------------------------
    // 1. The card. If today's fix regressed, `global_account()` would refuse
    //    with `NotGranted` before the credential was ever read, the run would
    //    fail AT ONCE, and neither `connect` nor `cancel` would ever appear —
    //    this `waitFor` is the first half of that regression guard.
    // ---------------------------------------------------------------------
    const card = page.locator('#cs-session-view [data-answer]:not([hidden])');
    const connect = card.locator('[data-action="answer-login"]');
    const cancel = card.locator('[data-action="answer-deny"]');
    await connect.waitFor({ state: 'visible', timeout: TURN_TIMEOUT });

    // The admin overlay's two rows: sign in now, or stop the task. The
    // non-admin card below withholds the first row entirely.
    await expect(connect).toHaveAttribute('marker', '1');
    await expect(cancel).toHaveAttribute('marker', '2');
    await expect(card.locator('.cs-answer-head')).toContainText(i18n('code_studio.ask.account.head'));
    await expect(connect).toHaveAttribute(
      'label', i18n('code_studio.ask.account.connect', { engine: 'Claude Code' }),
    );
    await expect(cancel).toHaveAttribute('label', i18n('code_studio.ask.account.cancel'));
    await expect(card.locator('.cs-answer-head .who')).toContainText('code-implementer');
    await expect(card.locator('.cs-answer-head .who')).toContainText('Claude Code');
    await expect(card.locator('.cs-answer-head .who')).toContainText(i18n('agent_accounts.subtitle_global'));

    // The exact FILLED body the product ships for an administrator looking at
    // a shared account that needs signing in again.
    const expectedQuestion = i18n('code_studio.ask.account.body_global_admin', {
      engine: 'Claude Code', account: SHARED_ACCOUNT_NAME,
    });
    await expect(card.locator('.cs-answer-q')).toHaveText(expectedQuestion);

    // ---------------------------------------------------------------------
    // 2. The stream anchor line: the Polish anchor, and NOT the server's own
    //    English prompt (`prompt_for_account` in `delegate_cli.rs`, carried
    //    on the approval row as `summary`) — the console never echoes that
    //    redacted-prose field on this line, and a leaked "uses your ...
    //    account" would mean it had started doing exactly that.
    //
    //    KNOWN PRODUCT RACE (not asserted around here — see the note below):
    //    `askMarkNode` builds the agent/engine part of this exact line
    //    (`code-implementer · Claude Code · konto globalne`) SYNCHRONOUSLY,
    //    by looking `ev.p.approval_id` up in `state.approvals`
    //    (`code-studio-session.js`, `askMarkNode` / `accountAskFromApproval`,
    //    called from `ingestEvents` → `buildEventNode`). That array is filled
    //    by a SEPARATE `loadApprovals()` the same event only triggers
    //    reactively, and the node is never patched once inserted (no code
    //    path re-renders an existing `.ev-askmark` by its `data-approval`).
    //    Measured directly (`document.querySelectorAll('.ev-askmark')` dumped
    //    mid-run): even re-entering the session, which drives the ORDERED
    //    mount path (`loadApprovals()` awaited before `loadTimeline()`
    //    replays the backlog), only renders the agent/engine part MOST of the
    //    time — the line came back empty except for the two static labels in
    //    roughly one run in three across repeated full-project runs, on both
    //    the admin and the member test. So the "who" part of this line is
    //    genuinely non-deterministic in the shipped code today, and an
    //    assertion pinned to it would make this suite flaky for a reason the
    //    product owns, not the test. The reliable "who" is the ANSWER CARD
    //    a few lines above (`.cs-answer-head .who`, driven by `state.ask`,
    //    which `loadApprovals()` sets directly with no separate lookup) —
    //    already asserted to contain the agent, the engine and the mode.
    // ---------------------------------------------------------------------
    const askMark = page.locator('#cs-session-view .ev-askmark').last();
    await askMark.waitFor({ state: 'visible', timeout: 30_000 });
    await expect(askMark).toContainText(i18n('code_studio.ask.account.anchor'));
    expect(await askMark.textContent()).not.toMatch(/uses your/i);

    // ---------------------------------------------------------------------
    // 3. What the SERVER wrote: a real account is named (unlike a `NotGranted`
    //    refusal, which names none at all — see `describe_agent_account`).
    // ---------------------------------------------------------------------
    const open = await pendingAccounts(page, globalWorkspaceId, adminGlobalSessionId);
    expect(open, 'expected exactly one open account card').toHaveLength(1);
    const row = open[0];
    expect(row.capability).toBe('account_login');
    expect(row.mandatory_interactive, 'the account card is not a permission').toBe(false);
    expect(row.account.engine_id).toBe('claude-code');
    expect(row.account.engine_name).toBe('Claude Code');
    expect(row.account.mode).toBe('global');
    expect(row.account.agent_name).toBe('code-implementer');
    expect(row.account.account_id).toBe(sharedAccountId);
    expect(row.account.account_name).toBe(SHARED_ACCOUNT_NAME);

    // ---------------------------------------------------------------------
    // 4. The admin ends the task too, so no run is left parked behind this
    //    file. The settled run's own note is the second half of the
    //    regression guard: `account_grant_denied` is the code a reverted fix
    //    would produce, and it must never appear here.
    // ---------------------------------------------------------------------
    await cancel.click();
    await expect.poll(async () => (
      await pendingAccounts(page, globalWorkspaceId, adminGlobalSessionId)).length, {
      timeout: 60_000, message: 'the admin card stayed pending after cancelling',
    }).toBe(0);
    await expect(connect).toHaveCount(0);

    let adminChild = null;
    await expect.poll(async () => {
      const body = await api(page, 'codeStudioSessionRunsRequest', scope(globalWorkspaceId, adminGlobalSessionId));
      adminChild = (body?.runs ?? []).find(
        (r) => (r.parentRunId ?? r.parent_run_id) && (r.agentId ?? r.agent_id) === globalImplementerId,
      );
      return adminChild?.status ?? 'missing';
    }, { timeout: 90_000, message: "the admin's delegated run never settled after the deny" })
      .toBe('failed');
    expect(adminChild.note, 'the failed run carries no reason').toBeTruthy();
    expect(adminChild.note).not.toContain('account_grant_denied');
    expect(adminChild.note).toContain('could not be delegated (C01)');
    expect(String(adminChild.note)).toContain('Claude Code');
    expect(adminChild.account, 'a run that never started a CLI named an account').toBeFalsy();
  });

  test('zwykły użytkownik widzi kartę bez logowania, a anulowanie kończy zadanie odmową', async ({ page }) => {
    test.setTimeout(TURN_TIMEOUT + 180_000);
    await prepare(page);
    await loginAsAdmin(page, {
      username: GLOBAL_MEMBER_USERNAME,
      password: GLOBAL_MEMBER_INITIAL_PASSWORD,
      port: PORT,
      rotateTo: GLOBAL_MEMBER_PASSWORD,
    });
    await openStudio(page);
    await openWorkspace(page, WORKSPACE_GLOBAL);
    memberGlobalSessionId = await currentSession(page, globalWorkspaceId);
    // Private per user (§5.3): the member's own session, never the admin's.
    expect(memberGlobalSessionId).not.toBe(adminGlobalSessionId);

    const composer = page.locator('#cs-session-view textarea').first();
    await composer.fill(TASK);
    await composer.press('Enter');

    // ---------------------------------------------------------------------
    // 1. The card, with the sign-in row withheld: `askOptions` returns only
    //    the cancel row for `mode==='global' && !ctx.isAdmin` — a member
    //    cannot sign a shared account in, so offering that button would
    //    promise an action the server refuses too.
    // ---------------------------------------------------------------------
    const card = page.locator('#cs-session-view [data-answer]:not([hidden])');
    const connect = card.locator('[data-action="answer-login"]');
    const cancel = card.locator('[data-action="answer-deny"]');
    await cancel.waitFor({ state: 'visible', timeout: TURN_TIMEOUT });

    await expect(connect).toHaveCount(0);
    await expect(cancel).toHaveAttribute('marker', '1');
    await expect(cancel).toHaveAttribute('label', i18n('code_studio.ask.account.cancel'));
    await expect(card.locator('.cs-answer-head')).toContainText(i18n('code_studio.ask.account.head'));
    await expect(card.locator('.cs-answer-head .who')).toContainText('code-implementer');
    await expect(card.locator('.cs-answer-head .who')).toContainText('Claude Code');

    // The exact FILLED body for a non-administrator: no self-service remedy,
    // only "ask an administrator" — a different sentence from the admin's own
    // card above, not a shorter render of the same one.
    const expectedQuestion = i18n('code_studio.ask.account.body_global', {
      engine: 'Claude Code', account: SHARED_ACCOUNT_NAME,
    });
    await expect(card.locator('.cs-answer-q')).toHaveText(expectedQuestion);

    // The stream anchor line: the static Polish label, and NOT the server's
    // English prompt. The agent/engine part of this same line is a known,
    // non-deterministic product race (see the admin test's comment above) and
    // is deliberately not asserted here for the same reason — the reliable
    // "who" is the answer card just asserted above.
    const askMark = page.locator('#cs-session-view .ev-askmark').last();
    await askMark.waitFor({ state: 'visible', timeout: 30_000 });
    await expect(askMark).toContainText(i18n('code_studio.ask.account.anchor'));
    expect(await askMark.textContent()).not.toMatch(/uses your/i);

    // ---------------------------------------------------------------------
    // 2. What the SERVER wrote for this principal — same account, same
    //    engine, the member's own approval row.
    // ---------------------------------------------------------------------
    const open = await pendingAccounts(page, globalWorkspaceId, memberGlobalSessionId);
    expect(open, 'expected exactly one open account card').toHaveLength(1);
    const row = open[0];
    expect(row.capability).toBe('account_login');
    expect(row.account.engine_id).toBe('claude-code');
    expect(row.account.mode).toBe('global');
    expect(row.account.agent_name).toBe('code-implementer');
    expect(row.account.account_id).toBe(sharedAccountId);
    expect(row.account.account_name).toBe(SHARED_ACCOUNT_NAME);

    // ---------------------------------------------------------------------
    // 3. The member stops the turn — the only action their card offers.
    // ---------------------------------------------------------------------
    await cancel.click();
    await expect.poll(async () => (
      await pendingAccounts(page, globalWorkspaceId, memberGlobalSessionId)).length, {
      timeout: 60_000, message: "the member's card stayed pending after cancelling",
    }).toBe(0);

    let memberChild = null;
    await expect.poll(async () => {
      const body = await api(page, 'codeStudioSessionRunsRequest', scope(globalWorkspaceId, memberGlobalSessionId));
      memberChild = (body?.runs ?? []).find(
        (r) => (r.parentRunId ?? r.parent_run_id) && (r.agentId ?? r.agent_id) === globalImplementerId,
      );
      return memberChild?.status ?? 'missing';
    }, { timeout: 90_000, message: "the member's delegated run never settled after the deny" })
      .toBe('failed');
    expect(memberChild.note, 'the failed run carries no reason').toBeTruthy();
    // THE REGRESSION THIS TEST GUARDS, on the member's own run: a member has
    // no self-service path around `account_grant_denied` at all (they cannot
    // sign a shared account in), so a regression here would strand every
    // member behind a silent, unfixable refusal instead of the C01 card
    // asserted above — the card is the proof the refusal never happened.
    expect(memberChild.note).not.toContain('account_grant_denied');
    expect(memberChild.note).toContain('could not be delegated (C01)');
    expect(String(memberChild.note)).toContain('Claude Code');

    // ---------------------------------------------------------------------
    // 4. The same facts in the node's own database, scoped to the member's
    //    own session (the workspace database also holds the admin's row from
    //    the sibling test, so the filter is by `session_id`, not by table).
    // ---------------------------------------------------------------------
    const dbRows = sql(
      `SELECT capability, status, decision, target_pattern, decided_by, account_json
       FROM approvals WHERE session_id = ${quote(memberGlobalSessionId)}
         AND capability = 'account_login'`,
      wsDb(globalWorkspaceId),
    );
    expect(dbRows, 'no account question row in the workspace database').toHaveLength(1);
    expect(dbRows[0].status).toBe('decided');
    expect(dbRows[0].decision).toBe('deny');
    expect(dbRows[0].target_pattern).toBe('claude-code');
    expect(dbRows[0].decided_by, 'the decision is attributed to a person').toBeTruthy();
    expect(JSON.parse(dbRows[0].account_json)).toEqual({
      engine_id: 'claude-code',
      engine_name: 'Claude Code',
      mode: 'global',
      agent_name: 'code-implementer',
      account_id: sharedAccountId,
      account_name: SHARED_ACCOUNT_NAME,
    });
  });
});
