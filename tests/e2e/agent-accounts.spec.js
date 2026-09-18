// =============================================================================
// File: tests/e2e/agent-accounts.spec.js
// Description: End-to-end suite for the agent-account screens
//              (mockups/agent-accounts-20260917: A01 list, A02 sign-in wizard,
//              A03/A04 account window, N01 node matrix, U01 "Moje konta",
//              G01 agent runtime).
//              Boots an isolated tentaflow instance and drives the dashboard
//              the way an administrator and a plain user would.
//
//              What is under test is the EFFECT, not the paint: after every
//              action the suite reads the node's own SQLite database (the
//              instance writes it right here on disk) and re-reads the same
//              object through the binary protocol after a FULL page reload. A
//              screen that congratulates the operator on a write the node never
//              made — the class of defect that shipped in `key-clear` — fails
//              here even though every pixel looks right.
// =============================================================================

const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');

const PORT = 18311;
// NOT under os.tmpdir(): /tmp is a RAM tmpfs on the dev machines and a booting
// instance unpacks container bundles into TENTAFLOW_HOME.
const WORK_DIR = path.join(__dirname, '../../.runtime/e2e-agent-accounts');
const DB = path.join(WORK_DIR, 'accounts.db');
const HOME = path.join(WORK_DIR, 'home');
const WWW_DIR = path.join(__dirname, '../../tentaflow-core/www');

const ACCOUNT_NAME = 'Claude Code — firma E2E';
const RENAMED = `${ACCOUNT_NAME} v2`;
const SPARE_NAME = 'Codex — do usunięcia';
const MEMBER_USERNAME = 'anna-e2e';
const MEMBER_DISPLAY = 'Anna Kowalska';
const MEMBER_INITIAL_PASSWORD = 'anna12345';
const MEMBER_PASSWORD = 'anna-e2e-2026';
const MEMBER_ACCOUNT_NAME = 'Codex — Anna';
// A shared account that authenticates through the provider's own sign-in — the
// only kind A02 applies to — and a personal one of the same kind for U01.
const LOGIN_NAME = 'Claude Code — logowanie E2E';
const MEMBER_LOGIN_ACCOUNT = 'Claude Code — Anna';
const SESSION_ID = 'sess-e2e-0001';
const GROUP_NAME = 'QA E2E';
const AGENT_NAME = 'Generator scenariuszy';
// `name` is the agent's kebab-case handle; the display name is what the card shows.
const AGENT_SLUG = 'generator-scenariuszy';
const FIRST_KEY = 'sk-ant-e2e-0000';
const SECOND_KEY = 'sk-ant-e2e-1111';
// A fresh node forces every account off its initial password before it lets
// anyone in, so the suite walks that screen once per account and then keeps
// using the rotated password.
const ADMIN_PASSWORD = 'admin-e2e-2026';
let adminPassword = 'admin';

let server = null;

test.beforeAll(async () => {
  test.skip(!binaryExists(), 'tentaflow binary not built (target_shared/{release-fast,release,debug})');
  fs.rmSync(WORK_DIR, { recursive: true, force: true });
  fs.mkdirSync(WORK_DIR, { recursive: true });
  server = startBinary({ port: PORT, db: DB, home: HOME, env: { TENTAFLOW_WWW_DIR: WWW_DIR } });
  await waitForServer(PORT, 60000);
});

test.afterAll(async () => {
  if (!server) return;
  const exited = new Promise((resolve) => server.once('exit', resolve));
  stopBinary(server);
  await Promise.race([exited, new Promise((r) => setTimeout(r, 10000))]);
  server = null;
});

// =============================================================================
// The node's own state — read from the database the instance is writing
// =============================================================================

// A reader alongside the running instance: SQLite in WAL mode admits readers
// while the server writes, and a busy timeout covers the moment of a commit.
function sql(query) {
  const out = execFileSync('/usr/bin/sqlite3', ['-json', '-cmd', '.timeout 5000', DB, query], {
    encoding: 'utf8',
  }).trim();
  return out ? JSON.parse(out) : [];
}

function quote(value) {
  return `'${String(value).replace(/'/g, "''")}'`;
}

function dbAccount(accountId) {
  return sql(`SELECT * FROM provider_accounts WHERE account_id = ${quote(accountId)}`)[0] ?? null;
}

function dbAccountByName(displayName) {
  return sql(`SELECT * FROM provider_accounts WHERE display_name = ${quote(displayName)}`)[0] ?? null;
}

function dbCredential(accountId) {
  return sql(
    `SELECT revision, material_sha256, material_enc FROM provider_account_credentials
     WHERE account_id = ${quote(accountId)}`,
  )[0] ?? null;
}

function dbGrants(accountId) {
  return sql(
    `SELECT subject_type, subject_id FROM provider_account_grants
     WHERE account_id = ${quote(accountId)} ORDER BY subject_type, subject_id`,
  );
}

function dbAudit(action, resource) {
  return sql(
    `SELECT action, resource, details FROM audit_log
     WHERE action = ${quote(action)} AND resource = ${quote(resource)} ORDER BY id`,
  );
}

function dbRuntimeNodes() {
  return sql('SELECT node_id, receives_accounts FROM agent_runtime_nodes ORDER BY node_id');
}

function dbAgentRuntime(name) {
  return sql(`SELECT runtime_json FROM agents WHERE name = ${quote(name)}`)[0]?.runtime_json ?? null;
}

function dbSessions(accountId) {
  return sql(
    `SELECT session_id, node_id, user_id FROM provider_account_sessions
     WHERE account_id = ${quote(accountId)} ORDER BY session_id`,
  );
}

function dbRuntimeEngines() {
  return sql('SELECT node_id, engine_id, install_state, version FROM agent_runtime_engines ORDER BY engine_id');
}

// Two rows nothing in the dashboard can create: a CLI session is opened by the
// bridge when an agent runs, and a node's credential state is written by the
// materializer. Both are seeded straight into the node's database so the
// screens that READ them (A03 sessions, "Używane na") can be driven for real.
function seed(statement) {
  execFileSync('/usr/bin/sqlite3', ['-cmd', '.timeout 5000', DB, statement], { encoding: 'utf8' });
}

function seedSession(accountId, sessionId, nodeId, userId) {
  seed(
    `INSERT INTO provider_account_sessions (account_id, session_id, user_id, node_id, started_at)
     VALUES (${quote(accountId)}, ${quote(sessionId)}, ${quote(userId)}, ${quote(nodeId)},
             strftime('%Y-%m-%dT%H:%M:%SZ','now','-14 minutes'))`,
  );
}

function seedMaterialized(accountId, nodeId) {
  seed(
    `INSERT OR REPLACE INTO provider_account_node_state
       (account_id, node_id, applied_revision, runtime_state)
     VALUES (${quote(accountId)}, ${quote(nodeId)}, 1, 'ready')`,
  );
}

// =============================================================================
// Browser
// =============================================================================

// Two init-time fixes, both applied before the app boots:
//   - `TENTAFLOW_WWW_DIR` hashes the frontend per request, so the instance
//     announces a "new version" modal that would swallow every click;
//   - a fresh instance defaults to English, while every string asserted here is
//     the Polish copy the mockups are written in.
async function prepare(page, jwt = null) {
  await page.addInitScript((token) => {
    localStorage.setItem('tentaflow_lang', 'pl');
    if (token) localStorage.setItem('tentaflow_jwt', token);
    const kill = () => document.querySelectorAll('.update-overlay').forEach((el) => el.remove());
    document.addEventListener('DOMContentLoaded', () => {
      kill();
      new MutationObserver(kill).observe(document.documentElement, { childList: true, subtree: true });
    });
  }, jwt);
}

// The node refuses more than ten logins a minute for one username, and this
// suite has more tests than that. A signed-in session is carried over the way a
// returning operator's browser carries it: the JWT the first login produced.
const sessions = new Map();

async function signIn(page, username, password, rotateTo) {
  const jwt = sessions.get(username);
  if (jwt) {
    await prepare(page, jwt);
    await page.goto(`https://127.0.0.1:${PORT}/`);
    await page.waitForSelector('aside', { timeout: 30000 });
    return false;
  }
  const rotated = await login(page, username, password, rotateTo);
  sessions.set(username, await page.evaluate(() => localStorage.getItem('tentaflow_jwt')));
  return rotated;
}

// Returns true when the initial password had to be rotated, so the caller can
// keep using the new one for the rest of the run.
async function login(page, username, password, rotateTo) {
  await prepare(page);
  await page.goto(`https://127.0.0.1:${PORT}/`);
  await page.locator('#login-username input').first().waitFor({ state: 'visible', timeout: 20000 });
  await page.locator('#login-username input').first().fill(username);
  await page.locator('#login-password input').first().fill(password);
  await page.locator('#login-submit').click();
  await Promise.race([
    page.waitForSelector('aside', { timeout: 30000 }),
    page.waitForSelector('#login-new-password input', { timeout: 30000 }),
  ]);
  if (await page.locator('#login-new-password input').count() === 0) return false;
  await page.locator('#login-password input').first().fill(password);
  await page.locator('#login-new-password input').first().fill(rotateTo);
  await page.locator('#login-confirm-password input').first().fill(rotateTo);
  await page.locator('#login-submit').click();
  await page.waitForSelector('aside', { timeout: 30000 });
  return true;
}

async function loginAsAdmin(page) {
  if (await signIn(page, 'admin', adminPassword, ADMIN_PASSWORD)) adminPassword = ADMIN_PASSWORD;
}

// The same binary protocol the screens use — for seeding a directory entry and
// for re-reading an object the way the next page load would.
async function api(page, action, payload) {
  return page.evaluate(async ([a, p]) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return p === null ? ApiBinary.one(a) : ApiBinary.action(a, p);
  }, [action, payload === undefined ? null : payload]);
}

/** Re-reads one account through the protocol on a FRESH page load. */
async function accountAfterReload(page, accountId) {
  await page.reload();
  await page.waitForSelector('aside', { timeout: 30000 });
  const response = await api(page, 'providerAccountGetRequest', { accountId });
  return response.account;
}

async function openAccountsTab(page) {
  await page.locator('.sidebar .nav-item[data-view="services"]').first().click();
  await page.waitForSelector('#svc-tabs tf-tab', { timeout: 20000 });
  await page.locator('#svc-tabs tf-tab#accounts').click();
  await page.waitForSelector('#aa-accounts-table', { timeout: 20000 });
}

/** Opens the account window from the list (a global account is row-clickable). */
async function openAccountWindow(page, name) {
  await page.locator('#aa-accounts-table').evaluate((table, wanted) => {
    const row = [...table.shadowRoot.querySelectorAll('tbody tr')]
      .find((tr) => tr.textContent.includes(wanted));
    if (!row) throw new Error(`row not found: ${wanted}`);
    row.click();
  }, name);
  const win = page.locator('tf-window').filter({ has: page.locator('.aa-detail') }).last();
  await win.locator('.aa-detail').waitFor({ state: 'visible', timeout: 15000 });
  return win;
}

/**
 * Creates an account through the A01 window, the way an administrator does.
 * Without a `key` the account is created for the provider's own sign-in, which
 * is the kind A02 exists for.
 */
async function createAccountThroughUi(page, { engine, name, key = null }) {
  await page.locator('#svc-tab-body tf-button[data-act="create"]').click();
  const win = page.locator('tf-window').filter({ has: page.locator('.aa-form') }).last();
  await win.locator('[data-field="engine"]').waitFor({ state: 'visible', timeout: 10000 });
  await win.locator('[data-field="engine"] select').selectOption(engine);
  await win.locator('[data-field="name"] input').fill(name);
  if (key === null) {
    await win.locator('[data-field="kind"] .tf-seg-opt[data-value="provider_login"]').click();
  } else {
    await win.locator('[data-field="kind"] .tf-seg-opt[data-value="api_key"]').click();
    await win.locator('[data-field="key"] input').fill(key);
  }
  await win.locator('tf-button[data-act="create"]').click();
  await page.locator('tf-window .aa-detail').waitFor({ state: 'visible', timeout: 15000 });
  await page.keyboard.press('Escape');
}

/** The node the browser is talking to, as the matrix reports it. */
async function localRuntimeNode(page) {
  const runtime = await api(page, 'providerAccountRuntimeListRequest', {});
  return runtime.nodes[0];
}

async function setReceivesAccounts(page, nodeId, enabled) {
  await api(page, 'providerAccountRuntimeSetReceivesAccountsRequest', { nodeId, enabled });
}

/**
 * Presses one of a list row's action buttons by its label. The buttons live in
 * the table's shadow root, and `HTMLElement.click()` dispatches a composed
 * event, so this is the same path a pointer takes.
 */
async function clickRowAction(page, rowText, label) {
  await page.locator('#aa-accounts-table').evaluate((table, [wanted, action]) => {
    const row = [...table.shadowRoot.querySelectorAll('tbody tr')]
      .find((tr) => tr.textContent.includes(wanted));
    if (!row) throw new Error(`row not found: ${wanted}`);
    const button = [...row.querySelectorAll('tf-button')]
      .find((el) => el.textContent.trim() === action);
    if (!button) throw new Error(`no "${action}" action on row ${wanted}`);
    button.click();
  }, [rowText, label]);
}

/** Opens the matrix cell's menu and picks one of its two actions. */
async function runtimeMenuAction(page, engineId, action) {
  await page.locator('#aa-runtime-table').evaluate((table, engine) => {
    const trigger = table.shadowRoot.querySelector(`tf-button[data-engine="${engine}"]`);
    if (!trigger) throw new Error(`no menu trigger for ${engine}`);
    if (trigger.hasAttribute('disabled')) throw new Error(`the ${engine} cell is disabled on this node`);
    trigger.click();
  }, engineId);
  const menu = page.locator('#aa-runtime-menu');
  await expect(menu).toHaveAttribute('open', '');
  const item = menu.locator(`tf-menu-item[action="${action}"]`);
  expect(await item.getAttribute('disabled'), `the "${action}" entry is disabled`).toBeNull();
  // tf-menu-item builds the row it listens on; the host element itself has no
  // click handler, so the pointer goes where a person's would.
  await item.locator('.tf-menu-item').click();
}

async function openRuntimeSegment(page) {
  await openAccountsTab(page);
  await page.locator('#svc-tab-body [data-field="segment"] .tf-seg-opt[data-value="runtime"]').click();
  await page.waitForSelector('#aa-runtime-table', { timeout: 15000 });
}

// tf-table renders its rows in a shadow root, so cell text has to be read from
// inside it rather than through a light-DOM locator.
function tableText(page, selector) {
  return page.locator(selector).evaluate((t) => t.shadowRoot?.textContent || '');
}

// Data rows only: an empty tf-table still renders one row, the "why it is
// empty" line, which would otherwise count as a result.
function tableRows(page, selector) {
  return page.locator(selector).evaluate(
    (t) => [...(t.shadowRoot?.querySelectorAll('tbody tr') ?? [])]
      .filter((tr) => !tr.classList.contains('tf-table__empty-row')).length,
  );
}

test.describe.configure({ mode: 'serial' });

test.describe('Konta agentów — administrator (A01, A03, A04)', () => {
  test('creating an account writes the account and its key, and both come back after a reload', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    await openAccountsTab(page);
    await createAccountThroughUi(page, { engine: 'claude-code', name: ACCOUNT_NAME, key: FIRST_KEY });

    await expect.poll(() => tableText(page, '#aa-accounts-table')).toContain(ACCOUNT_NAME);
    expect(await tableText(page, '#aa-accounts-table')).toContain('Globalne');
    expect(await tableText(page, '#aa-accounts-table')).toContain('Klucz API');
    await expect(page.locator('#aa-accounts-foot')).toContainText('1 konto');

    // The node's own row, not the one the screen is showing.
    const row = dbAccountByName(ACCOUNT_NAME);
    expect(row, 'provider_accounts row').toBeTruthy();
    expect(row.scope).toBe('global');
    expect(row.engine_id).toBe('claude-code');
    expect(row.credential_kind).toBe('api_key');
    expect(row.owner_user_id).toBeNull();
    const credential = dbCredential(row.account_id);
    expect(credential, 'the key was stored').toBeTruthy();
    expect(credential.revision).toBe(1);
    expect(credential.material_enc).not.toContain(FIRST_KEY);

    const reloaded = await accountAfterReload(page, row.account_id);
    expect(reloaded.display_name).toBe(ACCOUNT_NAME);
    expect(reloaded.credential_revision).toBe(1);
  });

  test('renaming persists in the database and survives a reload', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    const accountId = dbAccountByName(ACCOUNT_NAME).account_id;
    await openAccountsTab(page);
    const win = await openAccountWindow(page, ACCOUNT_NAME);

    await win.locator('[data-field="name"] input').fill(RENAMED);
    await win.locator('tf-button[data-act="rename"]').click();
    await expect.poll(() => dbAccount(accountId).display_name).toBe(RENAMED);

    const reloaded = await accountAfterReload(page, accountId);
    expect(reloaded.display_name).toBe(RENAMED);
  });

  test('replacing the API key bumps the stored revision and its fingerprint', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    const accountId = dbAccountByName(RENAMED).account_id;
    const before = dbCredential(accountId);
    await openAccountsTab(page);
    const win = await openAccountWindow(page, RENAMED);

    await win.locator('[data-field="key"] input').fill(SECOND_KEY);
    await win.locator('tf-button[data-act="key-save"]').click();
    await expect.poll(() => dbCredential(accountId).revision).toBe(before.revision + 1);
    expect(dbCredential(accountId).material_sha256).not.toBe(before.material_sha256);

    const reloaded = await accountAfterReload(page, accountId);
    expect(reloaded.credential_revision).toBe(before.revision + 1);
  });

  test('clearing the API key removes the credential row and writes the audit entry', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    const accountId = dbAccountByName(RENAMED).account_id;
    expect(dbCredential(accountId), 'there is a key to clear').toBeTruthy();
    await openAccountsTab(page);
    const win = await openAccountWindow(page, RENAMED);

    await win.locator('tf-button[data-act="key-clear"]').click();
    const confirm = page.locator('tf-window').filter({ hasText: 'Usunąć klucz API?' }).last();
    await confirm.locator('tf-button[data-action="confirm"]').click();

    // The success toast may not lie: the row has to be gone from the node.
    await expect.poll(() => dbCredential(accountId)).toBeNull();
    const audit = dbAudit('provider_account.credential_clear', accountId);
    expect(audit.length, 'one audit entry for the clear').toBe(1);
    expect(dbAccount(accountId).status).toBe('needs_login');

    const reloaded = await accountAfterReload(page, accountId);
    expect(reloaded.credential_revision).toBe(0);
    expect(reloaded.status).toBe('needs_login');
  });

  test('the enable switch writes the status and restores a truthful one', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    const accountId = dbAccountByName(RENAMED).account_id;
    await openAccountsTab(page);
    const win = await openAccountWindow(page, RENAMED);

    await win.locator('[data-field="enabled"] .tf-toggle').click();
    await expect.poll(() => dbAccount(accountId).status).toBe('disabled');

    await win.locator('[data-field="enabled"] .tf-toggle').click();
    // No credential is stored any more, so "enabled" must NOT mean "active".
    await expect.poll(() => dbAccount(accountId).status).toBe('pending');

    const reloaded = await accountAfterReload(page, accountId);
    expect(reloaded.status).toBe('pending');
  });

  test('granting a user and a group, then the whole organisation, replaces the stored grants', async ({ page }) => {
    test.setTimeout(180_000);
    await loginAsAdmin(page);
    await api(page, 'iamCreateUserRequest', {
      username: MEMBER_USERNAME, password: MEMBER_INITIAL_PASSWORD, displayName: MEMBER_DISPLAY,
      email: 'anna@e2e.local', role: 'user', groupIds: [],
    });
    await api(page, 'iamCreateGroupRequest', { name: GROUP_NAME, description: 'e2e' });
    const accountId = dbAccountByName(RENAMED).account_id;

    await openAccountsTab(page);
    const win = await openAccountWindow(page, RENAMED);
    await win.locator('tf-tab#access').click();
    await win.locator('[data-table="grants"]').waitFor({ state: 'visible', timeout: 10000 });
    for (const label of [MEMBER_DISPLAY, GROUP_NAME]) {
      const picker = win.locator('[data-field="subject"]');
      await picker.locator('.tf-combobox-input').click();
      await picker.locator('.tf-combobox-input').fill(label);
      await picker.locator('.tf-combobox-option', { hasText: label }).first().click();
    }
    await win.locator('tf-button[data-act="save-access"]').click();
    await expect.poll(() => dbGrants(accountId).map((g) => g.subject_type)).toEqual(['group', 'user']);

    // Re-read through the protocol on a fresh load, then widen to the whole org.
    await page.reload();
    await page.waitForSelector('aside', { timeout: 30000 });
    const fromWire = await api(page, 'providerAccountGetRequest', { accountId });
    expect(fromWire.grants.map((g) => g.subject_type).sort()).toEqual(['group', 'user']);

    await openAccountsTab(page);
    const reopened = await openAccountWindow(page, RENAMED);
    await reopened.locator('tf-tab#access').click();
    await reopened.locator('[data-field="mode"] .tf-seg-opt[data-value="org"]').click();
    await reopened.locator('tf-button[data-act="save-access"]').click();
    // A full replace: the user and the group are gone, one org row is left.
    await expect.poll(() => dbGrants(accountId)).toEqual([
      { subject_type: 'org', subject_id: expect.anything() },
    ]);
    await expect(reopened.locator('tf-tab#access')).toHaveAttribute('count', '1');
  });

  test('the toolbar narrows the list by text, application and scope', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    await openAccountsTab(page);
    await createAccountThroughUi(page, { engine: 'codex', name: SPARE_NAME, key: 'sk-e2e-codex' });
    await expect.poll(() => tableRows(page, '#aa-accounts-table')).toBe(2);

    await page.locator('#svc-tab-body [data-field="query"] input').fill('Codex');
    await expect.poll(() => tableRows(page, '#aa-accounts-table')).toBe(1);
    expect(await tableText(page, '#aa-accounts-table')).toContain(SPARE_NAME);

    await page.locator('#svc-tab-body [data-field="query"] input').fill('');
    await expect.poll(() => tableRows(page, '#aa-accounts-table')).toBe(2);
    await page.locator('#svc-tab-body [data-field="engine"] select').selectOption('claude-code');
    await expect.poll(() => tableRows(page, '#aa-accounts-table')).toBe(1);
    expect(await tableText(page, '#aa-accounts-table')).toContain(RENAMED);

    await page.locator('#svc-tab-body [data-field="engine"] select').selectOption('');
    await page.locator('#svc-tab-body [data-field="scope"] select').selectOption('user');
    // Neither account is personal, so the scope filter empties the list.
    await expect.poll(() => tableRows(page, '#aa-accounts-table')).toBe(0);
    expect(await tableText(page, '#aa-accounts-table')).toContain('Brak kont agentów');
  });

  test('deleting an account removes it from the node', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    const accountId = dbAccountByName(SPARE_NAME).account_id;
    await openAccountsTab(page);
    const win = await openAccountWindow(page, SPARE_NAME);

    await win.locator('tf-button[data-act="delete"]').click();
    const confirm = page.locator('tf-window').filter({ hasText: 'Usunąć konto?' }).last();
    await confirm.locator('tf-button[data-action="confirm"]').click();

    await expect.poll(() => dbAccount(accountId)).toBeNull();
    expect(dbCredential(accountId), 'the key went with it').toBeNull();
    await page.reload();
    await page.waitForSelector('aside', { timeout: 30000 });
    const list = await api(page, 'providerAccountListRequest', { engineId: null, scope: null, query: null });
    expect(list.accounts.some((a) => a.account_id === accountId)).toBe(false);
  });

  test('a seeded session is listed and "Zakończ" removes it from the node', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    const accountId = dbAccountByName(RENAMED).account_id;
    const node = await localRuntimeNode(page);
    // The administrator who granted access — a user id this node can resolve.
    const owner = sql(
      `SELECT granted_by FROM provider_account_grants WHERE account_id = ${quote(accountId)} LIMIT 1`,
    )[0]?.granted_by;
    expect(owner, 'the access test left a grant to take a user id from').toBeTruthy();
    seedSession(accountId, SESSION_ID, node.node_id, owner);
    expect(dbSessions(accountId).length).toBe(1);

    await openAccountsTab(page);
    const win = await openAccountWindow(page, RENAMED);
    const sessions = win.locator('[data-table="sessions"]');
    await expect.poll(() => sessions.evaluate(
      (t) => [...(t.shadowRoot?.querySelectorAll('tbody tr') ?? [])]
        .filter((tr) => !tr.classList.contains('tf-table__empty-row')).length,
    )).toBe(1);
    await expect(win.locator('.aa-sub-h').filter({ hasText: 'Aktywne sesje' })).toHaveText('Aktywne sesje (1)');

    await sessions.evaluate((table) => {
      const button = [...table.shadowRoot.querySelectorAll('tbody tr tf-button')]
        .find((el) => el.textContent.trim() === 'Zakończ');
      if (!button) throw new Error('no "Zakończ" action on the session row');
      button.click();
    });
    const confirm = page.locator('tf-window').filter({ hasText: 'Zakończyć sesję?' }).last();
    await confirm.locator('tf-button[data-action="confirm"]').click();

    // The node's own row is gone, and the list the window redrew says so.
    await expect.poll(() => dbSessions(accountId).length).toBe(0);
    await expect.poll(() => sessions.evaluate((t) => t.shadowRoot?.textContent || ''))
      .toContain('Brak aktywnych sesji');
    await page.keyboard.press('Escape');
  });

  test('"Używane na" and "Nadał" show what the answering node measured and who granted access', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    const accountId = dbAccountByName(RENAMED).account_id;
    const node = await localRuntimeNode(page);
    seedMaterialized(accountId, node.node_id);

    await openAccountsTab(page);
    // A01: the cell names the node, and the wire is what it took the name from.
    const list = await api(page, 'providerAccountListRequest', { engineId: null, scope: null, query: null });
    const wireAccount = list.accounts.find((a) => a.account_id === accountId);
    expect(wireAccount.used_on.map((n) => n.node_id)).toEqual([node.node_id]);
    await expect.poll(() => tableText(page, '#aa-accounts-table')).toContain(wireAccount.used_on[0].node_name);

    const win = await openAccountWindow(page, RENAMED);
    await expect(win.locator('.aa-kv')).toContainText(wireAccount.used_on[0].node_name);
    await expect(win.locator('.aa-kv dd[title]')).toHaveAttribute(
      'title', 'Zgodnie z pomiarem węzła, który odpowiedział.',
    );

    // A04: the org grant the access test left carries who granted it and when.
    const wire = await api(page, 'providerAccountGetRequest', { accountId });
    expect(wire.grants.length).toBeGreaterThan(0);
    expect(wire.grants[0].granted_at, 'the node records when a grant was made').toBeTruthy();
    await win.locator('tf-tab#access').click();
    const grants = win.locator('[data-table="grants"]');
    await grants.waitFor({ state: 'visible', timeout: 10000 });
    await expect.poll(() => grants.evaluate((t) => t.shadowRoot?.textContent || '')).toContain('Nadał');
    if (wire.grants[0].granted_by_name) {
      expect(await grants.evaluate((t) => t.shadowRoot?.textContent || ''))
        .toContain(wire.grants[0].granted_by_name);
    }
    await page.keyboard.press('Escape');
  });
});

// =============================================================================
// A02 — the provider sign-in
//
// The flow runs a vendor CLI ON THE NODE, so what can be proved here is
// everything up to the moment that process would talk to a provider: the two
// refusals the node itself decides (a node excluded from accounts, an
// environment that cannot run the CLI), the wizard's own wiring, and that
// nothing is written behind a sign-in that never succeeded. A completed
// sign-in needs a browser at the provider and is out of reach of this suite.
// =============================================================================

test.describe('Logowanie u dostawcy (A02)', () => {
  test('a node that does not receive accounts refuses the sign-in, in the operator\'s language', async ({ page }) => {
    test.setTimeout(150_000);
    await loginAsAdmin(page);
    await openAccountsTab(page);
    await createAccountThroughUi(page, { engine: 'claude-code', name: LOGIN_NAME });
    const account = dbAccountByName(LOGIN_NAME);
    expect(account.credential_kind).toBe('provider_login');
    expect(account.status).toBe('pending');

    const node = await localRuntimeNode(page);
    await setReceivesAccounts(page, node.node_id, false);
    await openAccountsTab(page);

    await clickRowAction(page, LOGIN_NAME, 'Zaloguj');
    const win = page.locator('tf-window').filter({ has: page.locator('.aa-login') }).last();
    await win.locator('.aa-login').waitFor({ state: 'visible', timeout: 10000 });
    // No node receives accounts, so the wizard offers no node to choose.
    await expect(win.locator('[data-field="node"]')).toHaveCount(0);
    await win.locator('tf-button[data-act="start"]').click();

    await expect(win.locator('[data-error]')).toBeVisible();
    await expect(win.locator('[data-error]')).toContainText('Ten węzeł nie przyjmuje kont agentowych');
    // A refusal is not an address: nothing linkable was shown.
    expect(await win.locator('[data-link]').evaluate((el) => el.hidden)).toBe(true);
    expect(dbCredential(account.account_id), 'a refused sign-in stores nothing').toBeNull();
    expect(dbAccount(account.account_id).status).toBe('pending');
    await page.keyboard.press('Escape');
  });

  test('on a node that does receive them, the sign-in fails where the environment does and says why', async ({ page }) => {
    test.setTimeout(150_000);
    await loginAsAdmin(page);
    const account = dbAccountByName(LOGIN_NAME);
    const node = await localRuntimeNode(page);
    await setReceivesAccounts(page, node.node_id, true);
    await openAccountsTab(page);

    await clickRowAction(page, LOGIN_NAME, 'Zaloguj');
    const win = page.locator('tf-window').filter({ has: page.locator('.aa-login') }).last();
    await win.locator('.aa-login').waitFor({ state: 'visible', timeout: 10000 });
    // The node now receives accounts, so it is offered as the place to run it.
    await expect(win.locator('[data-field="node"]')).toHaveCount(1);
    await win.locator('tf-button[data-act="start"]').click();

    // Whatever this machine lacks (a runtime that was never installed, a CLI
    // that cannot reach the provider), the reason travels to the operator —
    // and never as the transport's own `protocol error <Code>:` prefix.
    await expect(win.locator('[data-error]')).toBeVisible({ timeout: 60000 });
    const message = (await win.locator('[data-error]').textContent()).trim();
    expect(message.length).toBeGreaterThan(0);
    expect(message).not.toContain('protocol error');
    expect(await win.locator('[data-link]').evaluate((el) => el.hidden)).toBe(true);
    // The step 4 line repeats the outcome instead of leaving "waiting" behind.
    await expect(win.locator('[data-result]')).toHaveText(message);
    // Nothing was written: no credential, no status change, no session.
    expect(dbCredential(account.account_id)).toBeNull();
    expect(dbAccount(account.account_id).status).toBe('pending');
    expect(dbSessions(account.account_id).length).toBe(0);

    await page.keyboard.press('Escape');
    // Closing the wizard leaves the list usable and the account untouched.
    await expect.poll(() => tableText(page, '#aa-accounts-table')).toContain(LOGIN_NAME);

    // Put the matrix back the way this suite found it: a node that receives no
    // accounts and has no engine installed keeps NO row at all, which is the
    // starting point the N01 toggle test measures against.
    await setReceivesAccounts(page, node.node_id, false);
    expect(dbRuntimeNodes().length).toBe(0);
  });

  test('cancelling a sign-in this node has no record of is refused as unknown', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    // The id names a node and a flow; neither exists here. `NotFound` is what
    // the wizard turns into "this sign-in no longer exists on the node" when a
    // flow it was polling disappears under it.
    const outcome = await page.evaluate(async () => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      try {
        await ApiBinary.action('providerAccountLoginCancelRequest', {
          loginId: 'no-such-node:00000000-0000-0000-0000-000000000000',
        });
        return 'accepted';
      } catch (error) {
        return String(error?.message ?? error);
      }
    });
    expect(outcome).toContain('NotFound');
  });
});

test.describe('Aplikacje na nodach (N01)', () => {
  test('the receives-accounts switch is stored on the node row', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    await openAccountsTab(page);
    await page.locator('#svc-tab-body [data-field="segment"] .tf-seg-opt[data-value="runtime"]').click();
    await page.waitForSelector('#aa-runtime-table', { timeout: 15000 });
    expect(await tableRows(page, '#aa-runtime-table')).toBeGreaterThan(0);

    const before = await page.locator('#aa-runtime-table').evaluate(
      (t) => t.shadowRoot.querySelector('tf-toggle')?.hasAttribute('checked') ?? null,
    );
    expect(before).not.toBeNull();
    await page.locator('#aa-runtime-table').evaluate((t) => {
      // tf-toggle listens on the span it builds, not on the host.
      t.shadowRoot.querySelector('tf-toggle .tf-toggle').click();
    });
    const want = before ? 0 : 1;
    await expect.poll(() => dbRuntimeNodes()[0]?.receives_accounts).toBe(want);

    await page.reload();
    await page.waitForSelector('aside', { timeout: 30000 });
    const nodes = await api(page, 'providerAccountRuntimeListRequest', {});
    expect(nodes.nodes[0].receives_accounts).toBe(!before);
    await openAccountsTab(page);
    await page.locator('#svc-tab-body [data-field="segment"] .tf-seg-opt[data-value="runtime"]').click();
    await page.waitForSelector('#aa-runtime-table', { timeout: 15000 });
    expect(await page.locator('#aa-runtime-table').evaluate(
      (t) => t.shadowRoot.querySelector('tf-toggle')?.hasAttribute('checked'),
    )).toBe(!before);
  });

  test('the sub-line names the system the node reported and the account it holds', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    const node = await localRuntimeNode(page);
    // `os` is measured by the node itself; nothing replicates it, so the local
    // row is the one that can carry it — and the screen spells it out.
    expect(node.os, 'the local node reports its own system').toBeTruthy();
    const spelled = { linux: 'Linux', macos: 'macOS', windows: 'Windows' }[node.os] ?? node.os;
    await openRuntimeSegment(page);
    const text = await tableText(page, '#aa-runtime-table');
    expect(text).toContain(spelled);
    expect(text).toContain('Online');
    // The materialized row seeded earlier is what this node counts as an
    // account it holds; the plural comes from i18n, never from concatenation.
    expect(text).toContain('1 konto');
  });

  test('an installed application can be removed from the node through the cell menu', async ({ page }) => {
    test.setTimeout(150_000);
    await loginAsAdmin(page);
    const node = await localRuntimeNode(page);
    // A CLI this node really installed would need the vendor's package; the
    // row it WOULD write is seeded instead, so the "installed" cell and the
    // uninstall that follows are the real ones.
    seed(`INSERT OR IGNORE INTO agent_runtime_nodes (node_id) VALUES (${quote(node.node_id)})`);
    seed(
      `INSERT OR REPLACE INTO agent_runtime_engines (node_id, engine_id, install_state, version, installed_at)
       VALUES (${quote(node.node_id)}, 'codex', 'installed', '9.9.9-e2e', datetime('now'))`,
    );

    await openRuntimeSegment(page);
    await expect.poll(() => tableText(page, '#aa-runtime-table')).toContain('9.9.9-e2e');

    await runtimeMenuAction(page, 'codex', 'uninstall');
    await expect.poll(() => dbRuntimeEngines().filter((e) => e.engine_id === 'codex').length).toBe(0);
    await expect.poll(() => tableText(page, '#aa-runtime-table')).not.toContain('9.9.9-e2e');
    const nodes = await api(page, 'providerAccountRuntimeListRequest', {});
    expect((nodes.nodes[0].engines ?? []).some((e) => e.engine_id === 'codex')).toBe(false);
  });

  test('installing an application runs on the node and the matrix shows what it reported', async ({ page }) => {
    test.setTimeout(400_000);
    await loginAsAdmin(page);
    await openRuntimeSegment(page);
    expect(dbRuntimeEngines().filter((e) => e.engine_id === 'claude-code').length).toBe(0);

    await runtimeMenuAction(page, 'claude-code', 'install');
    // The node writes the row before it starts working, so this is the proof
    // the request reached the machine rather than the screen.
    await expect.poll(() => dbRuntimeEngines().find((e) => e.engine_id === 'claude-code')).toBeTruthy();
    const settled = async () => {
      const row = dbRuntimeEngines().find((e) => e.engine_id === 'claude-code');
      return row ? row.install_state : null;
    };
    await expect.poll(settled, { timeout: 300_000, intervals: [2000] }).not.toBe('installing');

    // Whatever this machine could do (a finished install, or the failure of one
    // without the vendor's package), the matrix redraws from the node's report.
    const row = dbRuntimeEngines().find((e) => e.engine_id === 'claude-code');
    const text = await tableText(page, '#aa-runtime-table');
    // An installed cell leads with the version the node recorded, and falls
    // back to the chip when it recorded none.
    if (row.install_state === 'installed') expect(text).toContain(row.version || 'Zainstalowana');
    else expect(text).toContain('Błąd');

    // Removing it again leaves the matrix as this suite found it.
    await runtimeMenuAction(page, 'claude-code', 'uninstall');
    await expect.poll(() => dbRuntimeEngines().filter((e) => e.engine_id === 'claude-code').length).toBe(0);
  });
});

test.describe('Moje konta — użytkownik (U01)', () => {
  test('a non-admin sees their own and granted accounts and no administration tab', async ({ page }) => {
    test.setTimeout(120_000);
    await signIn(page, MEMBER_USERNAME, MEMBER_INITIAL_PASSWORD, MEMBER_PASSWORD);

    // Serwisy is not in a plain user's sidebar at all, so the route is typed by
    // hand — which is exactly the case the tab removal in services.js covers.
    await expect(page.locator('.sidebar .nav-item[data-view="services"]')).toHaveCount(0);
    await page.goto(`https://127.0.0.1:${PORT}/#/services`);
    await page.waitForTimeout(3000);
    await expect(page.locator('#svc-tabs tf-tab#accounts')).toHaveCount(0);

    await page.goto(`https://127.0.0.1:${PORT}/#/my-accounts`);
    await page.waitForSelector('#myacc-apps .myapp-card', { timeout: 20000 });
    // The org-wide grant from the access test reaches her as a company account.
    await expect(page.locator('#myacc-apps')).toContainText('konto firmowe');
    await expect(page.locator('#myacc-apps [data-role="app-connect"]').first()).toBeVisible();

    const card = page.locator('#myacc-apps .myapp-card[data-engine="codex"]');
    await card.locator('[data-role="app-connect"]').click();
    const win = page.locator('tf-window').filter({ has: page.locator('.aa-form') }).last();
    await win.locator('[data-field="name"] input').fill(MEMBER_ACCOUNT_NAME);
    await win.locator('[data-field="kind"] .tf-seg-opt[data-value="api_key"]').click();
    await win.locator('[data-field="key"] input').fill('sk-e2e-anna');
    await win.locator('tf-button[data-act="create"]').click();

    // Hers, owned by her, with her key — read from the node, not from the card.
    await expect.poll(() => dbAccountByName(MEMBER_ACCOUNT_NAME)?.scope).toBe('user');
    const own = dbAccountByName(MEMBER_ACCOUNT_NAME);
    expect(own.owner_user_id).toBeTruthy();
    expect(dbCredential(own.account_id).revision).toBe(1);
    // The card leads with the account's NAME (mockup u01), not with the subtitle.
    await expect(card.locator('.linked-email')).toHaveText(MEMBER_ACCOUNT_NAME);
    await expect.poll(async () => card.textContent()).toContain('Odłącz');

    await page.reload();
    await page.waitForSelector('#myacc-apps .myapp-card', { timeout: 20000 });
    await expect(page.locator('#myacc-apps .myapp-card[data-engine="codex"]')).toContainText('Odłącz');
    const mine = await api(page, 'providerAccountMyListRequest', { engineId: null });
    expect(mine.accounts.some((a) => a.account_id === own.account_id)).toBe(true);
  });

  test('the card says how the account authenticates, where it is used and how many sessions it has', async ({ page }) => {
    test.setTimeout(120_000);
    await signIn(page, MEMBER_USERNAME, MEMBER_PASSWORD);
    const own = dbAccountByName(MEMBER_ACCOUNT_NAME);
    // Reading the node matrix is an administrator's right, so the id comes from
    // the node's own row instead of from a request this user may not make.
    const node = dbRuntimeNodes()[0];
    expect(node, 'the matrix tests left the node row').toBeTruthy();
    seedMaterialized(own.account_id, node.node_id);
    seedSession(own.account_id, 'sess-e2e-anna', node.node_id, own.owner_user_id);

    await page.goto(`https://127.0.0.1:${PORT}/#/my-accounts`);
    await page.waitForSelector('#myacc-apps .myapp-card', { timeout: 20000 });
    const card = page.locator('#myacc-apps .myapp-card[data-engine="codex"]');
    // Every one of the three comes off the wire, and the count is inflected by
    // i18n rather than glued to a fixed noun.
    const wire = await api(page, 'providerAccountMyListRequest', { engineId: null });
    const mine = wire.accounts.find((a) => a.account_id === own.account_id);
    expect(mine.credential_kind).toBe('api_key');
    expect(mine.used_on.map((n) => n.node_id)).toEqual([node.node_id]);
    expect(mine.session_count).toBe(1);
    await expect(card).toContainText('Klucz API');
    await expect(card).toContainText(`używane na: ${mine.used_on[0].node_name}`);
    await expect(card).toContainText('1 aktywna sesja');
    // An API-key account has nothing to sign in to.
    await expect(card.locator('[data-role="app-login"]')).toHaveCount(0);
  });

  test('connecting a subscription account opens the sign-in and leaves it offered on the card', async ({ page }) => {
    test.setTimeout(150_000);
    await signIn(page, MEMBER_USERNAME, MEMBER_PASSWORD);
    await page.goto(`https://127.0.0.1:${PORT}/#/my-accounts`);
    await page.waitForSelector('#myacc-apps .myapp-card', { timeout: 20000 });

    const card = page.locator('#myacc-apps .myapp-card[data-engine="claude-code"]');
    await card.locator('[data-role="app-connect"]').click();
    const form = page.locator('tf-window').filter({ has: page.locator('.aa-form') }).last();
    await form.locator('[data-field="name"] input').fill(MEMBER_LOGIN_ACCOUNT);
    await form.locator('[data-field="kind"] .tf-seg-opt[data-value="provider_login"]').click();
    await form.locator('tf-button[data-act="create"]').click();

    // Her account, hers to sign in — and the wizard opens on the spot instead
    // of leaving an account nobody told her to finish.
    await expect.poll(() => dbAccountByName(MEMBER_LOGIN_ACCOUNT)?.scope).toBe('user');
    const own = dbAccountByName(MEMBER_LOGIN_ACCOUNT);
    expect(own.credential_kind).toBe('provider_login');
    const wizard = page.locator('tf-window').filter({ has: page.locator('.aa-login') }).last();
    await wizard.locator('.aa-login').waitFor({ state: 'visible', timeout: 15000 });
    // A user may not read the node matrix, so Core picks the node for her.
    await expect(wizard.locator('[data-field="node"]')).toHaveCount(0);
    await page.keyboard.press('Escape');

    // Closing the wizard does not invent a credential, and the card keeps
    // offering the sign-in she has not finished.
    expect(dbCredential(own.account_id)).toBeNull();
    await expect.poll(() => card.textContent()).toContain('Zaloguj');
    const mine = await api(page, 'providerAccountMyListRequest', { engineId: null });
    expect(mine.accounts.find((a) => a.account_id === own.account_id).can_login).toBe(true);
  });
});

test.describe('Odmowa serwera (A03)', () => {
  test('an administrator renaming somebody else\'s personal account is refused, and the screen says so', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    const personal = dbAccountByName(MEMBER_ACCOUNT_NAME);
    expect(personal.scope).toBe('user');

    // A personal account is not row-clickable in A01 (the list says "tylko
    // właściciel"), so the window is opened the way its own module exports it.
    await page.evaluate(async (accountId) => {
      const [{ openAccountWindow }, { AgentAccounts }] = await Promise.all([
        import('/js/modules/agent-accounts-window.js'),
        import('/js/modules/agent-accounts.js'),
      ]);
      const list = await AgentAccounts.list({});
      await openAccountWindow(accountId, { engines: list.engines ?? [], isAdmin: true });
    }, personal.account_id);
    const win = page.locator('tf-window').filter({ has: page.locator('.aa-detail') }).last();
    await win.locator('.aa-detail').waitFor({ state: 'visible', timeout: 15000 });

    await win.locator('[data-field="name"] input').fill('Przejęte przez admina');
    const rename = win.locator('tf-button[data-act="rename"]');
    await rename.click();

    // The refusal has to be visible AND the button usable again — a stuck
    // disabled button is how a failed write looks like a hung screen.
    await expect(win.locator('[data-error]')).toBeVisible();
    await expect(page.locator('.toast.toast-error').last()).toBeVisible();
    await expect.poll(async () => rename.getAttribute('disabled')).toBeNull();
    expect(dbAccount(personal.account_id).display_name).toBe(MEMBER_ACCOUNT_NAME);
    await page.keyboard.press('Escape');
  });

  test('an administrator is offered no sign-in on somebody else\'s personal account, and refused one', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    const personal = dbAccountByName(MEMBER_LOGIN_ACCOUNT);
    expect(personal.scope).toBe('user');
    expect(personal.credential_kind).toBe('provider_login');

    // A01: her row carries no action at all, only the note that says why.
    await openAccountsTab(page);
    const rowActions = await page.locator('#aa-accounts-table').evaluate((table, wanted) => {
      const row = [...table.shadowRoot.querySelectorAll('tbody tr')]
        .find((tr) => tr.textContent.includes(wanted));
      if (!row) throw new Error(`row not found: ${wanted}`);
      return { text: row.textContent, buttons: row.querySelectorAll('tf-button').length };
    }, MEMBER_LOGIN_ACCOUNT);
    expect(rowActions.buttons).toBe(0);
    expect(rowActions.text).toContain('tylko właściciel');

    // A03 opened by hand offers no sign-in either — the footer has none.
    await page.evaluate(async (accountId) => {
      const [{ openAccountWindow }, { AgentAccounts }] = await Promise.all([
        import('/js/modules/agent-accounts-window.js'),
        import('/js/modules/agent-accounts.js'),
      ]);
      const list = await AgentAccounts.list({});
      await openAccountWindow(accountId, { engines: list.engines ?? [], isAdmin: true });
    }, personal.account_id);
    const win = page.locator('tf-window').filter({ has: page.locator('.aa-detail') }).last();
    await win.locator('.aa-detail').waitFor({ state: 'visible', timeout: 15000 });
    await expect(win.locator('tf-button[data-act="login"]')).toHaveCount(0);
    await page.keyboard.press('Escape');

    // And the node refuses the request even when it is made without a screen.
    const outcome = await page.evaluate(async (accountId) => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      try {
        await ApiBinary.one('providerAccountLoginStartRequest', { accountId, nodeId: null });
        return 'accepted';
      } catch (error) {
        return String(error?.message ?? error);
      }
    }, personal.account_id);
    expect(outcome).not.toBe('accepted');
    expect(dbCredential(personal.account_id)).toBeNull();
  });
});

// The card variant of tf-radio changed shape for the account picker (a <label>
// could not host the select that belongs to the option). ML Studio's project
// wizard is the OTHER screen built on those cards, so it is driven here — on a
// real instance, by pointer and by keyboard — rather than trusted to the unit
// test alone. Its own suites treat the type cards as optional and assert
// nothing about the selection.
test.describe('Karty tf-radio na drugim ekranie (kreator ML Studio)', () => {
  test('the project-type cards still select by click and by keyboard', async ({ page }) => {
    test.setTimeout(120_000);
    await loginAsAdmin(page);
    // ML Studio is a native app: on a fresh node its requests answer
    // "application is not installed", so the package is installed first, the
    // same way the catalogue screen does it.
    const install = await page.evaluate(async () => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      const packages = await ApiBinary.list('addonCatalogListRequest', { arrayKey: 'packages' });
      const pkg = packages.find((p) => (p.packageId ?? p.package_id) === 'ml-studio');
      if (!pkg) return { ok: false, error: 'ml-studio is not in the catalogue' };
      const installed = await ApiBinary.action('addonInstanceInstallRequest', {
        packageId: 'ml-studio',
        version: pkg.versions[0],
        displayName: pkg.name || 'ML Studio',
        config: [],
      });
      if (!installed.ok) return installed;
      // An instance lands disabled, and the app gate refuses a disabled app.
      const addons = await ApiBinary.list('addonsListRequest', { arrayKey: 'addons' });
      const instance = addons.find((a) => (a.packageId ?? a.package_id) === 'ml-studio');
      if (!instance) return { ok: false, error: 'the ml-studio instance is not listed' };
      await ApiBinary.action('addonToggleRequest', {
        addonId: instance.addonId ?? instance.addon_id,
        enabled: true,
      });
      return installed;
    });
    expect(install.ok, install.error || '').toBe(true);
    await page.goto(`https://127.0.0.1:${PORT}/#/ml-studio`);
    await page.waitForSelector('#ml-studio-new', { timeout: 20000 });
    await page.locator('#ml-studio-new').click();
    // Step 1 names the project, step 2 is the card picker under test.
    await page.locator('#ml-studio-wiz-name input').first().waitFor({ state: 'visible', timeout: 15000 });
    await page.locator('#ml-studio-wiz-name input').first().fill('Karty tf-radio E2E');
    await page.locator('#ml-studio-wiz-next').click();

    const group = page.locator('#ml-studio-wiz-types');
    await group.waitFor({ state: 'visible', timeout: 15000 });
    const cards = group.locator('.tf-radio-card-group__card');
    expect(await cards.count()).toBeGreaterThan(1);
    const values = await group.locator('tf-radio').evaluateAll((els) => els.map((el) => el.getAttribute('value')));

    // Pointer: the second card.
    await cards.nth(1).click();
    await expect(group).toHaveAttribute('value', values[1]);
    await expect(cards.nth(1)).toHaveClass(/tf-radio-card-group__card--selected/);

    // Keyboard: the card's radio marker carries the focus and the role, and it
    // is a zero-size box, so the key goes through the real focused element.
    const marker = group.locator('tf-radio').first().locator('.tf-radio-card-group__input');
    await expect(marker).toHaveAttribute('role', 'radio');
    await marker.evaluate((el) => el.focus());
    expect(await marker.evaluate((el) => document.activeElement === el)).toBe(true);
    await page.keyboard.press('Space');
    await expect(group).toHaveAttribute('value', values[0]);
    await expect(cards.nth(0)).toHaveClass(/tf-radio-card-group__card--selected/);
    await expect(cards.nth(1)).not.toHaveClass(/tf-radio-card-group__card--selected/);
  });
});

test.describe('Agent na aplikacji CLI (G01)', () => {
  test('a CLI runtime with a shared account round-trips, and so does the user mode', async ({ page }) => {
    test.setTimeout(180_000);
    await loginAsAdmin(page);
    // A fresh node has no agents, so the subject of this test is seeded through
    // the same upsert the wizard uses — its runtime is the default LLM one,
    // which is exactly the starting point the screen has to convert.
    await api(page, 'agentsUpsertRequest', {
      agentJson: JSON.stringify({
        name: AGENT_SLUG,
        display_name: AGENT_NAME,
        description: 'e2e',
        system_prompt: null,
        model: null,
        tools: [],
        skills: { names: [], tags: [] },
        params: {},
        max_iterations: 25,
        timeout_secs: 600,
        max_subagents: 0,
        max_spawn_depth: 1,
        on_child_complete: 'notify',
        flow_id: null,
        routable: true,
        is_enabled: true,
      }),
    });
    expect(JSON.parse(dbAgentRuntime(AGENT_SLUG))).toEqual({ kind: 'llm' });

    await page.locator('.sidebar .nav-item[data-view="agents"]').first().click();
    await page.waitForSelector('#agents-grid-host .agent-card', { timeout: 20000 });
    await page.locator('.agent-card').filter({ hasText: AGENT_NAME }).first().click();
    await page.waitForSelector('#agent-detail-tabs tf-tab', { timeout: 15000 });

    const body = page.locator('#ag-detail-body');
    await body.locator('[data-runtime-kind] .tf-seg-opt[data-value="cli"]').click();
    await body.locator('[data-runtime-pane="cli"]').waitFor({ state: 'visible', timeout: 10000 });
    await body.locator('[data-cfg="cli_engine"] select').selectOption('claude-code');
    // The effort levels are translated, never the raw wire ids.
    const levels = await body.locator('[data-cfg="cli_reasoning"] select').evaluate(
      (sel) => [...sel.options].map((o) => o.textContent.trim()),
    );
    expect(levels).toContain('Standardowa');
    await body.locator('[data-cfg="cli_reasoning"] select').selectOption('standard');
    await body.locator('[data-account-mode] tf-radio[value="global"] .tf-radio-card-group__card').click();
    const accountId = await body.locator('[data-cfg="account_id"] select').evaluate((sel) => {
      const option = [...sel.options].find((o) => o.value);
      if (!option) throw new Error('no shared account for claude-code');
      sel.value = option.value;
      sel.dispatchEvent(new Event('change', { bubbles: true }));
      return option.value;
    });
    expect(accountId).toBeTruthy();
    await body.locator('tf-button[data-draft-save]').click();
    await expect(page.locator('.ag-save-bar')).toHaveCount(0);

    // The exact JSON the node stored, not the draft still in memory.
    await expect.poll(() => JSON.parse(dbAgentRuntime(AGENT_SLUG))).toEqual({
      kind: 'cli',
      engine: 'claude-code',
      reasoning: 'standard',
      account: { mode: 'global', account_id: accountId },
    });

    await page.reload();
    await page.waitForSelector('aside', { timeout: 30000 });
    await page.locator('.sidebar .nav-item[data-view="agents"]').first().click();
    await page.locator('.agent-card').filter({ hasText: AGENT_NAME }).first().click();
    await page.waitForSelector('#agent-detail-tabs tf-tab', { timeout: 15000 });
    await expect(page.locator('#ag-detail-header-host .d-badges')).toContainText('Claude Code');
    const reopened = page.locator('#ag-detail-body');
    await expect(reopened.locator('[data-runtime-pane="cli"]')).toBeVisible();
    expect(await reopened.locator('[data-cfg="cli_engine"] select').inputValue()).toBe('claude-code');
    expect(await reopened.locator('[data-cfg="account_id"] select').inputValue()).toBe(accountId);

    // Switch to the runner's account: the binding must carry no account id.
    await reopened.locator('[data-account-mode] tf-radio[value="user"] .tf-radio-card-group__card').click();
    await reopened.locator('tf-button[data-draft-save]').click();
    await expect(page.locator('.ag-save-bar')).toHaveCount(0);
    await expect.poll(() => JSON.parse(dbAgentRuntime(AGENT_SLUG))).toEqual({
      kind: 'cli',
      engine: 'claude-code',
      reasoning: 'standard',
      account: { mode: 'user' },
    });

    await page.reload();
    await page.waitForSelector('aside', { timeout: 30000 });
    await page.locator('.sidebar .nav-item[data-view="agents"]').first().click();
    await page.locator('.agent-card').filter({ hasText: AGENT_NAME }).first().click();
    await page.waitForSelector('#agent-detail-tabs tf-tab', { timeout: 15000 });
    await expect(page.locator('#ag-detail-header-host .d-badges')).toContainText('Konto użytkownika');
    await expect(page.locator('#ag-detail-body [data-account-mode]')).toHaveAttribute('value', 'user');
  });
});
