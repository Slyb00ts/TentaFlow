// =============================================================================
// File: tests/e2e/helpers/code-studio.js
// Description: Operator actions on Code Studio for the e2e specs: protocol
//              calls through the dashboard's own shim, answering approvals,
//              creating a workspace through the wizard, opening a session and
//              accepting a review. Everything goes over the same wire the UI
//              uses, so a spec proves the product path, not a side door.
// =============================================================================

const { expect } = require('@playwright/test');
const { baseUrl } = require('./spawn');

/// Calls the binary protocol from page context through the shim the dashboard
/// uses. Unit requests (no payload) go through `one()`; passing them an object
/// shifts the sequence argument and the codec fails converting it to BigInt.
async function api(page, action, payload) {
  return page.evaluate(async ([a, p]) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return p === null ? ApiBinary.one(a) : ApiBinary.action(a, p);
  }, [action, payload === undefined ? null : payload]);
}

/// Registers an OpenAI-compatible endpoint as an external provider — the call
/// the deploy wizard makes — and waits until the node can route to it.
async function registerModelProvider(page, model, { repo = 'harness-test' } = {}) {
  const nodes = await api(page, 'meshNodeListRequest');
  const first = (nodes?.nodes ?? [])[0] ?? {};
  const res = await api(page, 'serviceManifestDeployRequest', {
    engineId: 'openai-compatible',
    deployMethod: 'external',
    nodeId: first.nodeId ?? first.node_id ?? '',
    configJson: JSON.stringify({
      base_url: model.baseUrl,
      api_key: 'harness-test-key',
      auth_mode: 'api',
      model_repo: repo,
    }),
  });
  expect(res?.deployId || res?.deploy_id, 'provider deploy returned no id').toBeTruthy();
  await expect.poll(async () => {
    const body = await api(page, 'serviceListRequest');
    const svc = (body?.services ?? []).find(
      (s) => (s.engineId ?? s.engine_id) === 'openai-compatible',
    );
    return svc?.status ?? 'missing';
  }, { timeout: 60_000, message: 'model provider never came up' }).toMatch(/running|degraded/);
}

/// Code Studio is a native APP: every request family passes the app gate, and a
/// node where the instance is missing or disabled answers `AppUnavailable`
/// before any handler runs. Installs and enables it from the node catalog — the
/// path the Addons screen takes — instead of writing a row the dashboard would
/// never create.
async function ensureCodeStudioApp(page) {
  const catalog = await api(page, 'addonCatalogListRequest');
  const pkg = (catalog?.packages ?? []).find(
    (p) => (p.packageId ?? p.package_id) === 'code-studio',
  );
  expect(pkg, 'the code-studio app is missing from the node catalog').toBeTruthy();
  const installed = await api(page, 'addonInstanceInstallRequest', {
    packageId: 'code-studio',
    version: String(pkg.latestVersion ?? pkg.latest_version ?? ''),
    displayName: String(pkg.name ?? 'Code Studio'),
    config: [],
  });
  expect(installed?.ok, `installing the code-studio app failed: ${installed?.error}`).toBe(true);
  const addonId = String(installed.addonId ?? installed.addon_id ?? '');
  expect(addonId, 'the install returned no instance id').toBeTruthy();
  const toggled = await api(page, 'addonToggleRequest', { addonId, enabled: true });
  expect(toggled?.ok, `enabling the code-studio app failed: ${toggled?.message}`).toBe(true);
}

/// Opens Code Studio by its deep link, falling back to the All apps tile.
async function gotoCodeStudio(page, port) {
  await page.goto(`${baseUrl(port)}/#/code-studio`);
  const direct = page.locator('#cs-new, #cs-empty-new, #cs-table-host').first();
  try {
    await direct.waitFor({ timeout: 30_000 });
  } catch {
    await page.goto(`${baseUrl(port)}/`);
    await page.locator('[data-view="apps-home"]').first().click();
    await page.locator('[data-route="code-studio"]').first().click();
    await page.waitForSelector('#cs-new, #cs-empty-new, #cs-table-host', { timeout: 30_000 });
  }
}

/// Grants the signed-in user the right to create workspaces (nobody holds it
/// on a fresh database), then walks the wizard with an empty repository and
/// waits for provisioning to finish. Returns the workspace id.
async function createWorkspace(page, port, name) {
  const me = await api(page, 'authMeRequest');
  const users = await api(page, 'usersListRequest');
  const mine = (users?.users ?? []).find((u) => (u.username ?? '') === (me?.username ?? 'admin'));
  const userId = mine?.id ?? mine?.userId ?? mine?.user_id ?? '';
  expect(userId, 'could not resolve the signed-in user id').toBeTruthy();
  await api(page, 'codeStudioWorkspaceCreatorGrantSetRequest', { userId, granted: true });

  await gotoCodeStudio(page, port);
  await page.locator('#cs-new, #cs-empty-new').first().click();
  await page.locator('#cs-wz-name input').first().fill(name);
  // Execution mode and source keep their defaults; the last `next` creates.
  for (let i = 0; i < 3; i += 1) await page.locator('[data-action="next"]').first().click();

  let workspaceId = '';
  await expect.poll(async () => {
    const body = await api(page, 'codeStudioWorkspacesListRequest', {});
    const ws = (body?.workspaces ?? []).find((w) => w.name === name);
    if (ws) workspaceId = ws.id ?? ws.workspaceId ?? '';
    return ws?.status ?? 'missing';
  }, { timeout: 60_000, message: 'workspace never reached active' }).toBe('active');
  return workspaceId;
}

/// Opens the workspace's chat the way an operator does: clicking the workspace
/// goes straight into its most recent open session, and opens a new one when it
/// has none. Returns the session id.
async function openSession(page, port, { workspaceName, workspaceId }) {
  await gotoCodeStudio(page, port);
  await page.locator(`text=${workspaceName}`).first().click();
  await page.waitForSelector('#cs-session-view', { state: 'visible', timeout: 30_000 });
  const list = await api(page, 'codeStudioSessionsListRequest', { workspaceId });
  const session = (list?.sessions ?? [])[0] ?? {};
  const sessionId = session.id ?? session.sessionId ?? '';
  expect(sessionId, 'no session id after opening the session view').toBeTruthy();
  return sessionId;
}

/// Answers every approval pending RIGHT NOW and returns how many. Meant to be
/// called inside polling predicates, so a long wait keeps the turn moving the
/// way an attentive operator would.
async function drainApprovals(page, scope, { decision = 'allow_for_run' } = {}) {
  const body = await api(page, 'codeStudioApprovalsListRequest', scope).catch(() => null);
  const pending = (body?.approvals ?? []).filter((a) => (a.status ?? '') === 'pending');
  for (const a of pending) {
    await api(page, 'codeStudioApprovalDecideRequest', {
      ...scope,
      approvalId: a.id ?? a.approvalId ?? a.approval_id,
      decision,
    }).catch(() => null);
  }
  return pending.length;
}

/// The session's runs as the dock hydrates them.
async function sessionRuns(page, scope) {
  const body = await api(page, 'codeStudioSessionRunsRequest', scope).catch(() => null);
  return body?.runs ?? [];
}

/// Accepts every hunk of every patch set still open for review — the call the
/// Changes pane makes. Returns the paths it accepted.
async function acceptOpenReviews(page, scope) {
  const body = await api(page, 'codeStudioPatchSetsListRequest', scope).catch(() => null);
  const sets = (body?.patchSets ?? body?.patch_sets ?? [])
    .filter((s) => ['open', 'in_review'].includes(s.status ?? ''));
  const accepted = [];
  for (const set of sets) {
    const patchSetId = set.patchSetId ?? set.patch_set_id;
    const detail = await api(page, 'codeStudioPatchSetGetRequest', { ...scope, patchSetId });
    const files = (detail?.files ?? []).map((f) => ({
      patchFileId: f.patch_file_id ?? f.patchFileId,
      decision: 'accept',
      hunks: (f.hunks ?? []).map((h) => ({
        patchHunkId: h.patch_hunk_id ?? h.patchHunkId,
        decision: 'accept',
      })),
    }));
    if (!files.length) continue;
    await api(page, 'codeStudioPatchDecideRequest', { ...scope, patchSetId, files });
    accepted.push(...(detail?.files ?? []).map((f) => f.path));
  }
  return accepted;
}

/// Reads a file from the session's worktree.
async function readWorktreeFile(page, scope, path) {
  const body = await api(page, 'codeStudioFileReadRequest', { ...scope, path });
  return String(body?.content ?? '');
}

module.exports = {
  api,
  ensureCodeStudioApp,
  registerModelProvider,
  gotoCodeStudio,
  createWorkspace,
  openSession,
  drainApprovals,
  sessionRuns,
  acceptOpenReviews,
  readWorktreeFile,
};
