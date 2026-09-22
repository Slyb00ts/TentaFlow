// =============================================================================
// File: tests/e2e/agent-accounts-mesh.spec.js
// Description: Cross-node agent-account credentials, live on two REAL nodes.
//              The account-administration suite (agent-accounts.spec.js)
//              proves single-node behaviour; this one stands up a second node,
//              pairs it over the real mesh (iroh LAN discovery + the PIN
//              handshake), and asserts the fan-out against both nodes' OWN
//              SQLite databases and the binary protocol — never against a
//              screen that merely looks right.
//
//              What is under test, per docs/agent-accounts-operations.md
//              §"Obecność konta na nodach" and the "Binding corrections" of
//              docs/agent-accounts-technical-design.md:
//                * a credential minted on the home node really lands on a
//                  satellite whose "Otrzymuje konta" flag is on;
//                * a satellite with the flag OFF never materializes one, and
//                  the refusals are the documented ones;
//                * pasting the SAME key on another node moves `home_node_id`;
//                * an equal-revision and an older-revision submission are both
//                  refused by the credential CAS, with the documented audits.
//
//              Two nodes are enough, and no vendor is ever contacted: every
//              account here is an `api_key` account whose key an administrator
//              pastes, and no bridge is installed on either node. That is also
//              the honest limit of this suite — see the note on
//              `provider_account_node_state` in the delivery test: an api_key
//              credential delivered to a satellite with no bridge writes NO
//              node-state row, so `applied_revision`/`runtime_state` are
//              asserted as ABSENT (the code has no path that could write them
//              here), not as reaching the account revision.
// =============================================================================

const { test, expect } = require('@playwright/test');
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

const {
  CONFIG_TEMPLATE,
  binaryExists,
  startBinary,
  stopBinary,
  waitForServer,
} = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');

const A_PORT = 18321;
const B_PORT = 18322;
const RUNTIME = path.join(__dirname, '../../.runtime/e2e-agent-accounts-mesh');
const A_DB = path.join(RUNTIME, 'a.db');
const B_DB = path.join(RUNTIME, 'b.db');
const ARTIFACTS = path.join(RUNTIME, 'artifacts');
// The frontend is served from the tree, not from the copy baked into the
// binary, so the screens under test are the ones this checkout builds.
const WWW_DIR = path.join(__dirname, '../../tentaflow-core/www');

const ENGINE = 'claude-code';
const ACC1 = 'Mesh delivery E2E';
const ACC2 = 'Mesh conflict E2E';
const ACC3 = 'Mesh stale E2E';
const ACC4 = 'Mesh sign-in E2E';
const KEY1 = 'sk-ant-mesh-0001';
const KEY2 = 'sk-ant-mesh-0002';
const KEY3 = 'sk-ant-mesh-0003';
const KEY4 = 'sk-ant-mesh-0004';
const KEY5 = 'sk-ant-mesh-0005';
const KEY6 = 'sk-ant-mesh-0006';
const KEY7 = 'sk-ant-mesh-0007';
const KEY8 = 'sk-ant-mesh-0008';
const KEY9 = 'sk-ant-mesh-0009';
const KEY10 = 'sk-ant-mesh-0010';

// The fan-out is only visible in the log at info, and RUST_LOG beats the config
// file's own level (tentaflow/src/main.rs appends BASE_FILTER to the env level),
// so the level is raised for one target instead of for the whole node.
const RUST_LOG = 'warn,tentaflow_core::mesh=info';

let nodeA = null;
let nodeB = null;
let aId = '';
let bId = '';
// The account the delivery test creates, kept across the tests that rotate it.
let acc1 = '';

// A login costs one of the ten attempts the core allows per username per
// minute, and this suite opens a page per test. The JWT the first login
// produced is carried over the way a returning operator's browser carries it.
const tokens = new Map();

function renderConfig(port) {
  const out = `/tmp/e2e-aa-mesh-${port}.toml`;
  const cfg = fs
    .readFileSync(CONFIG_TEMPLATE, 'utf8')
    .replace(/"0\.0\.0\.0:18099"/g, `"0.0.0.0:${port}"`)
    .replace(/^port = 18099$/m, `port = ${port}`)
    .replace(/^health_check_bind = "0\.0\.0\.0:19889"$/m, `health_check_bind = "0.0.0.0:${port + 1000}"`)
    .replace('[mesh]\nenabled = false', '[mesh]\nenabled = true')
    .replace('mdns_enabled = false', 'mdns_enabled = true');
  fs.writeFileSync(out, cfg);
  return out;
}

// =============================================================================
// Each node's own state — read from the database that node is writing
// =============================================================================

// A reader alongside a running instance: SQLite in WAL mode admits readers
// while the server writes, and a busy timeout covers the moment of a commit.
function sql(db, query) {
  const out = execFileSync('/usr/bin/sqlite3', ['-json', '-cmd', '.timeout 5000', db, query], {
    encoding: 'utf8',
  }).trim();
  return out ? JSON.parse(out) : [];
}

function quote(value) {
  return `'${String(value).replace(/'/g, "''")}'`;
}

function seed(db, statement) {
  execFileSync('/usr/bin/sqlite3', ['-cmd', '.timeout 5000', db, statement], { encoding: 'utf8' });
}

function dbAccount(db, accountId) {
  return sql(db, `SELECT * FROM provider_accounts WHERE account_id = ${quote(accountId)}`)[0] ?? null;
}

function dbAccountByName(db, displayName) {
  return sql(db, `SELECT * FROM provider_accounts WHERE display_name = ${quote(displayName)}`)[0] ?? null;
}

function dbCredential(db, accountId) {
  return (
    sql(
      db,
      `SELECT account_id, revision, material_sha256, material_enc, refreshed_by_node
       FROM provider_account_credentials WHERE account_id = ${quote(accountId)}`,
    )[0] ?? null
  );
}

function dbNodeState(db, accountId) {
  return sql(
    db,
    `SELECT account_id, node_id, applied_revision, runtime_state FROM provider_account_node_state
     WHERE account_id = ${quote(accountId)}`,
  );
}

// The raw row, not a boolean: whether the OFF branch DELETED the row or wrote a
// zero is exactly what the operator's decision leaves behind (`has_engines`),
// and the suite reports which of the two happened instead of flattening them.
function dbRuntimeNode(db, nodeId) {
  return (
    sql(db, `SELECT node_id, receives_accounts FROM agent_runtime_nodes WHERE node_id = ${quote(nodeId)}`)[0] ??
    null
  );
}

/// `agent_runtime_nodes` for a node this installation never wrote a row about
/// reads as NOT receiving — the store answers `false` for a missing row, which
/// is what the sender's publish gate consults.
function receivesAccounts(db, nodeId) {
  const row = dbRuntimeNode(db, nodeId);
  return row ? row.receives_accounts === 1 : false;
}

function dbAudit(db, action, resource) {
  return sql(
    db,
    `SELECT action, resource, resource_type, resource_id, details FROM audit_log
     WHERE action = ${quote(action)} AND resource = ${quote(resource)} ORDER BY id`,
  );
}

/// The parsed `details` of every audit row for one action, oldest first.
function auditDetails(db, action, resource) {
  return dbAudit(db, action, resource).map((row) => {
    try {
      return JSON.parse(row.details);
    } catch {
      return {};
    }
  });
}

function sha256Hex(text) {
  return crypto.createHash('sha256').update(text).digest('hex');
}

/// A short prefix, for a failure message that has to name WHICH material an
/// assertion saw without printing the credential's digest in full.
function shortSha(sha) {
  return String(sha ?? '').slice(0, 12);
}

// =============================================================================
// Browser
// =============================================================================

// `TENTAFLOW_WWW_DIR` hashes the frontend per request, so the instance
// announces a "new version" modal that would swallow every click; and a fresh
// instance defaults to English while every string asserted here is the Polish
// copy the mockups are written in. Both are applied before the app boots.
function prepare(page, jwt) {
  page.addInitScript((token) => {
    localStorage.setItem('tentaflow_lang', 'pl');
    if (token) localStorage.setItem('tentaflow_jwt', token);
    const kill = () => document.querySelectorAll('.update-overlay').forEach((el) => el.remove());
    document.addEventListener('DOMContentLoaded', () => {
      kill();
      new MutationObserver(kill).observe(document.documentElement, { childList: true, subtree: true });
    });
  }, jwt ?? null);
}

async function openPage(browser, port) {
  const page = await browser.newPage();
  const jwt = tokens.get(port);
  prepare(page, jwt ?? null);
  if (jwt) {
    await page.goto(`https://127.0.0.1:${port}/`);
    await page.waitForSelector('aside', { timeout: 30000 });
  } else {
    await loginAsAdmin(page, { port });
    tokens.set(port, await page.evaluate(() => localStorage.getItem('tentaflow_jwt')));
  }
  return page;
}

// The same binary protocol the screens use, over the same shim.
async function api(page, action, payload) {
  return page.evaluate(async ([a, p]) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return p === null ? ApiBinary.one(a) : ApiBinary.action(a, p);
  }, [action, payload === undefined ? null : payload]);
}

/// The same call, but resolving to the protocol's refusal instead of throwing —
/// an expected refusal is a result here, not an accident.
///
/// Two things have to happen inside the page, and neither survives the
/// Playwright boundary:
///   * the catch itself — what crosses the boundary is re-thrown as a plain
///     error;
///   * the code, because on the WebSocket transport a refused call is rejected
///     with `protocol error <Code>: <message>` and NO `code` property
///     (`protocol/binary-ws-client.js:472`). That is why the app's own
///     `describeError` parses the prefix rather than reading a field
///     (`modules/agent-accounts.js:207`); this helper does the same, so a spec
///     can assert the wire enum and the node's own sentence separately.
async function apiError(page, action, payload) {
  const raw = await page.evaluate(
    async ([a, p]) => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      try {
        await (p === null ? ApiBinary.one(a) : ApiBinary.action(a, p));
        return null;
      } catch (error) {
        return { code: error?.code ?? null, message: String(error?.message ?? error) };
      }
    },
    [action, payload === undefined ? null : payload],
  );
  if (raw === null) return null;
  const parsed = /^protocol error ([A-Za-z]+):\s*([\s\S]*)$/.exec(raw.message);
  return {
    code: raw.code ?? (parsed ? parsed[1] : null),
    message: parsed ? parsed[2].trim() : raw.message,
    raw: raw.message,
  };
}

/**
 * The two halves of the accounts tab, as its segmented control names them.
 *
 * The TAB and the SEGMENT are independent, and the segment outlives a re-mount
 * (the module keeps it): on a tab that is already active a click changes
 * nothing, so a caller coming back from the runtime matrix would wait forever
 * for the accounts table. The segment is therefore selected explicitly, not
 * assumed.
 */
async function selectSegment(page, segment) {
  const table = segment === 'runtime' ? '#aa-runtime-table' : '#aa-accounts-table';
  const option = page.locator(
    `#svc-tab-body [data-field="segment"] .tf-seg-opt[data-value="${segment}"]`,
  );
  if (await option.count()) await option.click();
  await page.waitForSelector(table, { timeout: 20000 });
  return table;
}

async function openAccountsTab(page) {
  await page.locator('.sidebar .nav-item[data-view="services"]').first().click();
  await page.waitForSelector('#svc-tabs tf-tab', { timeout: 20000 });
  await page.locator('#svc-tabs tf-tab#accounts').click();
  await selectSegment(page, 'accounts');
}

async function openRuntimeSegment(page) {
  await openAccountsTab(page);
  await selectSegment(page, 'runtime');
}

/// The answering node's own row. `node_catalog` puts the local node first, so
/// the matrix on node X describes X in `nodes[0]`, and `is_local` says so.
async function localRow(page) {
  const runtime = await api(page, 'providerAccountRuntimeListRequest', {});
  return runtime.nodes[0];
}

/// The matrix lists the node that answered first (`node_catalog` puts the local
/// id at the head of the order), so the first switch in that table is the one
/// the local operator owns. The premise is asserted rather than assumed: a
/// click that flipped a PEER's flag would otherwise be indistinguishable from
/// the refusal this suite is looking for.
async function toggleLocalReceives(page) {
  const local = await localRow(page);
  if (local?.is_local !== true) throw new Error('the first row of the matrix is not the local node');
  await page.locator('#aa-runtime-table').evaluate((table) => {
    // tf-toggle listens on the span it builds, not on the host.
    table.shadowRoot.querySelector('tf-toggle .tf-toggle').click();
  });
}

/**
 * The operator's decision about this node, in the direction a test needs.
 *
 * A switch already where it is asked to be is left alone rather than flipped
 * back: these tests hand state to one another, and a blind toggle would invert
 * whatever the previous one arranged.
 *
 * `peerDb`/`peerNodeId` also wait for the PEER's replica of the row. That
 * replica is what the fan-out gate on the SENDING side reads, so a test that
 * pastes a key straight after the switch would otherwise race the ledger — and
 * a key meant to be the satellite's first would arrive after the home's own.
 */
async function setLocalReceives(page, enabled, { peerDb, peerNodeId } = {}) {
  await openRuntimeSegment(page);
  const local = await localRow(page);
  if (local?.is_local !== true) throw new Error('the first row of the matrix is not the local node');
  if (local.receives_accounts !== enabled) {
    await toggleLocalReceives(page);
    await poll(async () => ((await localRow(page)).receives_accounts === enabled ? true : null), {
      timeoutMs: 30_000,
      what: `the local receives-accounts switch to read ${enabled}`,
    });
  }
  if (peerDb && peerNodeId) {
    await poll(async () => (receivesAccounts(peerDb, peerNodeId) === enabled ? true : null), {
      timeoutMs: 90_000,
      what: `the peer's replica of the switch to read ${enabled}`,
    });
  }
}

function localToggleChecked(page) {
  return page
    .locator('#aa-runtime-table')
    .evaluate((t) => t.shadowRoot.querySelector('tf-toggle')?.hasAttribute('checked') ?? null);
}

function tableText(page, selector) {
  return page.locator(selector).evaluate((t) => t.shadowRoot?.textContent || '');
}

/// The accounts screen, opened only when it is not already the one on screen: a
/// test that pastes a key after a `reload()` would otherwise click the sidebar
/// on top of a screen that is already there.
async function ensureAccountsTab(page) {
  if ((await page.locator('#aa-accounts-table').count()) === 0) await openAccountsTab(page);
}

/**
 * Creates an account through the A01 window, the way an administrator does.
 * A `key` also pastes it, so the account is created WITH a credential; without
 * one the account is the sign-in kind, which is what the fleet gate refuses.
 */
async function createAccountThroughUi(page, { engine, name, key = null }) {
  await ensureAccountsTab(page);
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
  await win.waitFor({ state: 'detached', timeout: 15000 }).catch(() => {});
}

/** Opens the account window from the list (a global account is row-clickable). */
async function openAccountWindow(page, name) {
  await ensureAccountsTab(page);
  await page.locator('#aa-accounts-table').evaluate((table, wanted) => {
    const row = [...table.shadowRoot.querySelectorAll('tbody tr')].find((tr) =>
      tr.textContent.includes(wanted),
    );
    if (!row) throw new Error(`row not found: ${wanted}`);
    row.click();
  }, name);
  const win = page.locator('tf-window').filter({ has: page.locator('.aa-detail') }).last();
  await win.locator('.aa-detail').waitFor({ state: 'visible', timeout: 15000 });
  return win;
}

/**
 * Pastes a key into an existing account — the operator gesture that both
 * rotates a credential and, on a different node, moves the home.
 */
async function pasteKey(page, name, key) {
  const win = await openAccountWindow(page, name);
  await win.locator('[data-field="key"] input').fill(key);
  await win.locator('tf-button[data-act="key-save"]').click();
  await page.keyboard.press('Escape');
  await win.waitFor({ state: 'detached', timeout: 15000 }).catch(() => {});
  await page.waitForTimeout(250);
}

/// Re-reads one account through the protocol on a FRESH page load: the answer
/// comes from the database, not from a module's in-memory copy.
async function accountAfterReload(page, accountId) {
  await page.reload();
  await page.waitForSelector('aside', { timeout: 30000 });
  const response = await api(page, 'providerAccountGetRequest', { accountId });
  return response.account;
}

function logText(proc) {
  return (proc?.logTail ?? []).join('');
}

// Waits for a value, so a slow discovery, a slow ledger or a slow frame is a
// wait rather than a race. `fn` answers the value or a falsy "not yet".
async function poll(fn, { timeoutMs = 60_000, what = 'condition' } = {}) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    try {
      last = await fn();
    } catch (error) {
      last = { error: String(error) };
    }
    if (last) return last;
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`timed out waiting for ${what}; last value: ${JSON.stringify(last)}`);
}

async function pollAbsent(fn, { holdMs = 5_000, what = 'value' } = {}) {
  const deadline = Date.now() + holdMs;
  while (Date.now() < deadline) {
    const found = await fn();
    if (found) throw new Error(`expected no ${what}, found ${JSON.stringify(found)}`);
    await new Promise((r) => setTimeout(r, 500));
  }
}

test.describe.configure({ mode: 'serial' });

test.describe('Agent accounts across two real mesh nodes', () => {
  test.beforeAll(async () => {
    test.skip(!binaryExists(), 'tentaflow binary not built (target_shared/{release-fast,release,debug})');
    fs.rmSync(RUNTIME, { recursive: true, force: true });
    fs.mkdirSync(ARTIFACTS, { recursive: true });
    nodeA = startBinary({
      port: A_PORT,
      db: A_DB,
      home: path.join(RUNTIME, 'a-home'),
      configFile: renderConfig(A_PORT),
      rustLog: RUST_LOG,
      env: { TENTAFLOW_WWW_DIR: WWW_DIR },
    });
    nodeB = startBinary({
      port: B_PORT,
      db: B_DB,
      home: path.join(RUNTIME, 'b-home'),
      configFile: renderConfig(B_PORT),
      rustLog: RUST_LOG,
      env: { TENTAFLOW_WWW_DIR: WWW_DIR },
    });
    await Promise.all([waitForServer(A_PORT, 60000), waitForServer(B_PORT, 60000)]);
  });

  test.afterAll(async () => {
    for (const [name, proc] of [
      ['node-a', nodeA],
      ['node-b', nodeB],
    ]) {
      if (!proc) continue;
      fs.writeFileSync(path.join(ARTIFACTS, `${name}.log`), logText(proc));
      const exited = new Promise((resolve) => proc.once('exit', resolve));
      stopBinary(proc);
      await Promise.race([exited, new Promise((r) => setTimeout(r, 10000))]);
    }
    nodeA = null;
    nodeB = null;
  });

  // ---------------------------------------------------------------------------
  // The mesh itself: without trust nothing below can be attributed to the
  // fan-out, so it is asserted first and separately.
  // ---------------------------------------------------------------------------
  test('two nodes trust each other after the real pairing handshake', async ({ browser }) => {
    const pageA = await openPage(browser, A_PORT);
    const pageB = await openPage(browser, B_PORT);

    const identityA = await api(pageA, 'meshIdentityRequest');
    const identityB = await api(pageB, 'meshIdentityRequest');
    aId = String(identityA?.nodeId ?? identityA?.node_id ?? '');
    bId = String(identityB?.nodeId ?? identityB?.node_id ?? '');
    expect(aId, 'the dashboard node has no mesh identity').toHaveLength(64);
    expect(bId, 'the peer node has no mesh identity').toHaveLength(64);

    // Discovery is the peer's OWN announcement — the catalog is built from the
    // peer store, so nothing below can pass without it.
    await poll(
      async () => {
        const list = await api(pageA, 'meshNodeListRequest');
        const ids = (list?.nodes ?? []).map((n) => String(n?.nodeId ?? n?.node_id ?? ''));
        return ids.includes(bId) ? ids : null;
      },
      { timeoutMs: 90_000, what: `node ${bId.slice(0, 12)} to be discovered by node A` },
    );

    const started = await api(pageA, 'meshPairingStartRequest', {
      remoteAddress: bId,
      pinHint: '',
      remotePublicKey: '',
      remoteAddresses: [],
      remoteRelayUrl: '',
      remoteHostname: '',
    });
    const pin = String(started?.pin ?? '');
    expect(pin, 'pairing produced no PIN').toHaveLength(6);

    // Node B sees the pending request that arrived over the wire and answers it
    // with the PIN, exactly like an operator reading the code aloud.
    const pending = await poll(
      async () => {
        const body = await api(pageB, 'meshPendingListRequest');
        return (
          (body?.pending ?? []).find((p) => String(p.remoteNodeId ?? p.remote_node_id) === aId) ?? null
        );
      },
      { timeoutMs: 90_000, what: 'the pending pairing to reach node B' },
    );

    const confirmed = await api(pageB, 'meshPairingConfirmRequest', {
      pairId: String(pending.remoteNodeId ?? pending.remote_node_id),
      pin,
    });
    expect(confirmed?.ok, 'the confirming node reported the pairing as done').toBe(true);
    expect(
      String(confirmed?.trustedNodeId ?? ''),
      'and named the node it now trusts',
    ).toBe(aId);

    for (const [page, wanted, other] of [
      [pageA, bId, 'A'],
      [pageB, aId, 'B'],
    ]) {
      await poll(
        async () => {
          const body = await api(page, 'meshTrustedListRequest');
          const trusted = (body?.trusted ?? []).map((n) => String(n?.nodeId ?? n?.node_id ?? ''));
          return trusted.includes(wanted) ? trusted : null;
        },
        { timeoutMs: 90_000, what: `node ${wanted.slice(0, 12)} to become trusted on node ${other}` },
      );
    }

    await pageA.close();
    await pageB.close();
  });

  // ---------------------------------------------------------------------------
  // 1) Delivery: a credential minted on the home node reaches a receiving
  //    satellite, and lands in that satellite's OWN store.
  // ---------------------------------------------------------------------------
  test('a credential minted on the home node reaches a receiving satellite', async ({ browser }) => {
    test.setTimeout(300_000);
    const pageA = await openPage(browser, A_PORT);
    const pageB = await openPage(browser, B_PORT);

    // The operator's decision, taken on each node's own N01 matrix.
    await openRuntimeSegment(pageA);
    await toggleLocalReceives(pageA);
    await poll(async () => ((await localRow(pageA)).receives_accounts === true ? true : null), {
      timeoutMs: 30_000,
      what: 'node A to accept accounts',
    });
    expect(receivesAccounts(A_DB, aId), 'node A recorded the decision').toBe(true);

    await openRuntimeSegment(pageB);
    await toggleLocalReceives(pageB);
    await poll(async () => ((await localRow(pageB)).receives_accounts === true ? true : null), {
      timeoutMs: 30_000,
      what: 'node B to accept accounts',
    });
    expect(receivesAccounts(B_DB, bId), 'node B recorded the decision').toBe(true);
    expect(
      auditDetails(B_DB, 'agent_runtime.receives_accounts', bId).map((d) => d.enabled),
      'the toggle is audited on the node that owns the decision',
    ).toEqual([true]);

    // The flag is a FLEET decision, and `agent_runtime_nodes` is a synced table:
    // node A's replica has to carry B's row before the sender's publish gate can
    // include B at all.
    await poll(async () => (receivesAccounts(A_DB, bId) === true ? true : null), {
      timeoutMs: 90_000,
      what: "node A's replica of node B's receives_accounts flag",
    });

    // An account created WITH its key: create and credential-set in one gesture.
    await openAccountsTab(pageA);
    await createAccountThroughUi(pageA, { engine: ENGINE, name: ACC1, key: KEY1 });
    acc1 = dbAccountByName(A_DB, ACC1)?.account_id ?? '';
    expect(acc1, 'the created account is not in node A\'s database').toBeTruthy();
    expect(dbCredential(A_DB, acc1)?.revision).toBe(1);

    // The account row is what the credential write is bound to on the receiving
    // side: an entry that arrives before the row is dropped silently, so the
    // suite waits for the ledger instead of racing it.
    await poll(async () => dbAccountByName(B_DB, ACC1), {
      timeoutMs: 120_000,
      what: 'the account row to reach node B through the sync ledger',
    });

    // A rotation is what guarantees a frame: the row is now on B, so the entry
    // has something to bind to.
    await pasteKey(pageA, ACC1, KEY2);
    const onA = dbCredential(A_DB, acc1);
    expect(onA.revision, 'the home minted a new revision').toBe(2);
    expect(onA.refreshed_by_node).toBe(aId);

    let delivered;
    try {
      delivered = await poll(async () => dbCredential(B_DB, acc1), {
        timeoutMs: 120_000,
        what: 'the credential to be delivered to node B',
      });
    } catch (error) {
      // A frame the receiver's structural validator rejects never reaches the
      // handler, so the symptom on this side is pure silence. Say what the two
      // nodes actually logged about the frame instead of only that nothing came.
      const about = logText(nodeB)
        .split('\n')
        .filter((line) => line.includes('ProviderCredentialsSync') || line.includes('rejected incoming frame'));
      throw new Error(
        `${error.message}\nnode B's log about that frame: ` +
          (about.slice(-3).join(' | ') || '(nothing — the frame was never logged)'),
      );
    }

    // The satellite's own row: the same revision and the SAME material, sealed
    // with that node's own cipher (so the stored bytes differ) and attributed to
    // the node that minted it.
    expect(delivered.revision, 'the satellite holds the delivered revision').toBe(onA.revision);
    expect(delivered.material_sha256, 'the delivered material is the minted one').toBe(
      onA.material_sha256,
    );
    expect(delivered.material_sha256, 'the stored material is the pasted key').toBe(sha256Hex(KEY2));
    expect(delivered.material_enc, 'the satellite re-seals with its own cipher').not.toBe(
      onA.material_enc,
    );
    expect(delivered.refreshed_by_node, 'the entry names the minting node').toBe(aId);

    await poll(
      async () => (logText(nodeB).includes('adopted newer account credentials') ? true : null),
      { timeoutMs: 60_000, what: 'node B to log the adoption of the delivered credential' },
    );

    // On the wire, from the satellite's own node: the account is in its catalog
    // and its home is still A.
    const wireB = await accountAfterReload(pageB, acc1);
    expect(wireB.account_id).toBe(acc1);
    expect(wireB.home_node_id).toBe(aId);

    // THE HONEST BOUNDARY of this item. `provider_account_node_state` is written
    // only by a bridge-backed materialization, and this node has no engine
    // installed, so a mesh-delivered api_key credential reaches
    // `provider_account_credentials` and NOTHING writes a node-state row: the
    // matrix keeps counting zero accounts for a node that does hold the
    // credential. The suite asserts the absence it can see rather than a
    // revision that no code path here could produce.
    expect(
      dbNodeState(B_DB, acc1),
      'no bridge ran here, so no node-state row may exist for the delivered credential',
    ).toHaveLength(0);
    const rowB = await localRow(pageB);
    expect(rowB.node_id).toBe(bId);
    expect(rowB.is_local, "the matrix row describes the node that answered").toBe(true);
    expect(rowB.account_count, 'the materialized count reads zero').toBe(0);
    await openRuntimeSegment(pageB);
    expect(
      await tableText(pageB, '#aa-runtime-table'),
      'the matrix says so on screen as well',
    ).toContain('brak kont');

    await pageA.close();
    await pageB.close();
  });

  // ---------------------------------------------------------------------------
  // 2) The gate — sender side: a node with the flag OFF is not in the
  //    publishable set at all, and the fleet sees the row disappear.
  // ---------------------------------------------------------------------------
  test('turning the receiving switch off is a fleet decision that stops the fan-out', async ({ browser }) => {
    test.setTimeout(240_000);
    const pageB = await openPage(browser, B_PORT);
    const pageA = await openPage(browser, A_PORT);

    expect(receivesAccounts(A_DB, bId), 'precondition: A counts B as a receiving node').toBe(true);

    await openRuntimeSegment(pageB);
    await toggleLocalReceives(pageB);
    await poll(async () => ((await localRow(pageB)).receives_accounts === false ? true : null), {
      timeoutMs: 30_000,
      what: 'node B to stop accepting accounts',
    });

    // Whether the row was deleted or zeroed depends on `has_engines`; the
    // decision itself is what has to hold.
    expect(receivesAccounts(B_DB, bId), 'node B no longer receives accounts').toBe(false);
    expect(
      auditDetails(B_DB, 'agent_runtime.receives_accounts', bId).map((d) => d.enabled),
      'both decisions are audited in order',
    ).toEqual([true, false]);

    // And the withdrawal replicates: node A's replica loses the row, which is
    // exactly what takes B out of `publishable_credentials`.
    await poll(async () => (receivesAccounts(A_DB, bId) === false ? true : null), {
      timeoutMs: 90_000,
      what: "node A's replica to lose node B's row",
    });

    await pageA.close();
    await pageB.close();
  });

  // ---------------------------------------------------------------------------
  // 2) The gate — receiver side: no materialization, the purge of what the
  //    satellite already held, and the documented refusal for a sign-in.
  // ---------------------------------------------------------------------------
  test('a satellite that does not receive accounts never materializes one', async ({ browser }) => {
    test.setTimeout(300_000);
    const pageA = await openPage(browser, A_PORT);
    const pageB = await openPage(browser, B_PORT);

    await openAccountsTab(pageA);
    await createAccountThroughUi(pageA, { engine: ENGINE, name: ACC2, key: KEY3 });
    const acc2 = dbAccountByName(A_DB, ACC2)?.account_id ?? '';
    expect(acc2, 'the second account is not in node A\'s database').toBeTruthy();
    const onA = dbCredential(A_DB, acc2);
    expect(onA.revision, 'the home minted revision 1 for it').toBe(1);
    expect(onA.material_sha256).toBe(sha256Hex(KEY3));

    // The satellite DOES see the account — the ledger replicates it — and must
    // still never see the credential. A missing row is the proof; an absent
    // success would not be.
    await poll(async () => dbAccountByName(B_DB, ACC2), {
      timeoutMs: 120_000,
      what: 'the second account row to reach node B',
    });
    expect(dbCredential(B_DB, acc2), 'node B holds no credential for it').toBeNull();

    // The purge is the documented consequence of the switch ("Poświadczenia,
    // które ten node zapisał wcześniej, są usuwane przy najbliższym
    // uzgodnieniu"): node B held ACC1's credential and takes it off its disk.
    const purged = await poll(async () => auditDetails(B_DB, 'provider_account.credential_purged', acc1).at(-1) ?? null, {
      timeoutMs: 120_000,
      what: 'the credential node B already held to be purged',
    });
    expect(purged.node_id, 'the purge names the node it happened on').toBe(bId);
    expect(purged.reason, 'the reason is the switch, not a revocation').toBe('not_receiving');
    expect(dbCredential(B_DB, acc1), 'the purged credential is gone from node B\'s store').toBeNull();

    // The home keeps minting: there IS something new to fan out, and the
    // satellite still gets nothing.
    await pasteKey(pageA, ACC1, KEY4);
    const rotated = dbCredential(A_DB, acc1);
    expect(rotated.revision, 'the home rotated past what node B used to hold').toBe(3);
    expect(rotated.material_sha256).toBe(sha256Hex(KEY4));
    await pollAbsent(() => dbCredential(B_DB, acc1), { holdMs: 6_000, what: 'delivered credential' });
    expect(dbCredential(B_DB, acc2)).toBeNull();

    // The documented refusal for the other operator path: starting a sign-in on
    // a node that does not receive accounts, without touching any vendor.
    //
    // It takes a `provider_login` account to reach it: for a pasted-key account
    // the same handler refuses EARLIER with BadRequest ("authenticates with a
    // key, so there is nothing to sign in to"), so the fleet gate is never
    // consulted — an account with no key at all is created for this one check.
    await createAccountThroughUi(pageA, { engine: ENGINE, name: ACC4 });
    const acc4 = dbAccountByName(A_DB, ACC4)?.account_id ?? '';
    expect(acc4, 'the sign-in account is not in node A\'s database').toBeTruthy();
    expect(dbAccount(A_DB, acc4)?.credential_kind, 'it is a provider sign-in account').toBe(
      'provider_login',
    );
    await poll(async () => dbAccountByName(B_DB, ACC4), {
      timeoutMs: 120_000,
      what: 'the sign-in account row to reach node B',
    });

    const refusal = await apiError(pageB, 'providerAccountLoginStartRequest', {
      accountId: acc4,
      nodeId: bId,
    });
    expect(refusal, 'the sign-in was not refused at all').not.toBeNull();
    expect(refusal.code).toBe('PolicyDenied');
    expect(refusal.message).toContain('is not configured to receive agent accounts');
    expect(dbCredential(B_DB, acc4), 'the refused sign-in stored nothing').toBeNull();

    await pageA.close();
    await pageB.close();
  });

  // ---------------------------------------------------------------------------
  // 2b) The gate — the PASTE path. A pasted key is the `api_key` twin of a
  //     sign-in: both write the credential onto THIS disk and both claim the
  //     home, so the same node decision refuses both — and it refuses the paste
  //     BEFORE the write, which is what keeps the account from being homed on a
  //     node the fleet will purge it from.
  // ---------------------------------------------------------------------------
  test('pasting a key on a node that does not receive accounts is refused like a sign-in', async ({ browser }) => {
    test.setTimeout(240_000);
    const pageB = await openPage(browser, B_PORT);

    // The state test 4 left the fleet in, asserted rather than assumed.
    expect(receivesAccounts(B_DB, bId), 'precondition: node B does not receive accounts').toBe(false);
    expect(receivesAccounts(A_DB, bId), 'precondition: node A knows it').toBe(false);

    const acc2 = dbAccountByName(A_DB, ACC2)?.account_id ?? '';
    expect(acc2, 'precondition: the second account exists').toBeTruthy();
    expect(dbCredential(B_DB, acc2), 'precondition: the satellite holds nothing').toBeNull();
    expect(
      dbAccount(B_DB, acc2)?.home_node_id,
      'precondition: node B is not this account\'s home',
    ).not.toBe(bId);
    const movesBefore = auditDetails(B_DB, 'provider_account.home_moved', acc2).length;

    // The operator's gesture, on the screen the operator uses. The window is
    // left open on purpose: the refusal is rendered IN it.
    const win = await openAccountWindow(pageB, ACC2);
    await win.locator('[data-field="key"] input').fill(KEY5);
    await win.locator('tf-button[data-act="key-save"]').click();

    // It says WHY they cannot have it, in their own language — the sentence the
    // sign-in refusal already answers with, because it is the same decision.
    const shown = await poll(
      async () => {
        const text = (await win.locator('p[data-error]').textContent().catch(() => '')) || '';
        return text.includes('Ten node nie przyjmuje kont agentowych') ? text : null;
      },
      { timeoutMs: 30_000, what: 'the refusal in the account window' },
    );
    expect(shown, 'the refusal names the remedy').toContain('Włącz je dla tego noda');

    // And on the wire: the code the GUI maps is the one the sign-in path uses,
    // so both operator paths render the same sentence.
    const refusal = await apiError(pageB, 'providerAccountCredentialSetRequest', {
      accountId: acc2,
      material: KEY5,
    });
    expect(refusal, 'the paste was not refused at all').not.toBeNull();
    expect(refusal.code).toBe('PolicyDenied');
    expect(refusal.message).toContain('is not configured to receive agent accounts');

    // The refusal came before any write, so there is nothing to undo.
    expect(dbCredential(B_DB, acc2), 'the refused paste minted a credential').toBeNull();
    expect(dbAccount(B_DB, acc2)?.home_node_id, 'the refused paste claimed the home').not.toBe(bId);
    expect(
      auditDetails(B_DB, 'provider_account.home_moved', acc2).length,
      'the refused paste recorded a move that never happened',
    ).toBe(movesBefore);

    await pageB.close();
  });

  // ---------------------------------------------------------------------------
  // 2c) The gate — receiver side, "a gdyby ją dostał, odrzuca całą ramkę": the
  //     frame is built and the receiving node refuses it WHOLE.
  //
  //     The trigger is a DIVERGENCE INJECTION, not an operator path: a raw SQL
  //     write into node A's replica of the fleet flag makes A believe B receives
  //     accounts, so A builds and sends a frame B would otherwise never be sent.
  //     It is the only way to reach the receiver's refusal with two nodes, since
  //     the sender's own gate is the thing that normally prevents it — and it is
  //     removed again at the end of the test, leaving the replica describing the
  //     fleet as it really is.
  //
  //     It runs here, BEFORE any paste, because a paste makes this node an
  //     account's home and the operator's switch is then refused (test 6): this
  //     is the last point in the suite where the node can be outside the fleet
  //     for real, and the real switch — not an injected replica — is what the
  //     receiver's refusal has to be shown against.
  // ---------------------------------------------------------------------------
  test('a frame a non-receiving node is sent anyway is refused whole', async ({ browser }) => {
    test.setTimeout(240_000);
    const pageA = await openPage(browser, A_PORT);

    expect(receivesAccounts(A_DB, bId), 'precondition: node A knows B does not receive').toBe(false);
    seed(
      A_DB,
      `INSERT INTO agent_runtime_nodes (node_id, receives_accounts) VALUES (${quote(bId)}, 1)`,
    );
    expect(receivesAccounts(A_DB, bId), 'the injected replica now claims B receives').toBe(true);

    const before = dbCredential(A_DB, acc1).revision;
    const logBefore = logText(nodeB).length;
    await pasteKey(pageA, ACC1, KEY9);
    expect(dbCredential(A_DB, acc1).revision, 'the home has new material to fan out').toBe(before + 1);

    // Both halves of the refusal, in node B's own log: the frame ARRIVED and was
    // rejected as a whole.
    const rejected = await poll(
      async () => {
        const tail = logText(nodeB).slice(logBefore);
        return tail.includes('ProviderCredentialsSync rejected') &&
          tail.includes('does not receive agent accounts')
          ? tail.split('\n').filter((l) => l.includes('ProviderCredentialsSync rejected')).join('\n')
          : null;
      },
      { timeoutMs: 120_000, what: 'node B to refuse the whole frame' },
    );
    expect(rejected).toContain('does not receive agent accounts');

    // Nothing was adopted: the refusal happens before the entries are read.
    expect(dbCredential(B_DB, acc1), 'the refused frame left no credential behind').toBeNull();

    seed(A_DB, `DELETE FROM agent_runtime_nodes WHERE node_id = ${quote(bId)}`);
    expect(receivesAccounts(A_DB, bId), 'the injected replica is gone again').toBe(false);

    await pageA.close();
  });

  // ---------------------------------------------------------------------------
  // 5) The CAS, equal revision: two writers minted the same number with
  //    different material, and the one that got there first keeps its copy.
  // ---------------------------------------------------------------------------
  test('an equal-revision submission with different material is refused as a conflict', async ({ browser }) => {
    test.setTimeout(300_000);
    const pageB = await openPage(browser, B_PORT);
    const pageA = await openPage(browser, A_PORT);

    const acc2 = dbAccountByName(A_DB, ACC2)?.account_id ?? '';
    const held = dbCredential(A_DB, acc2);
    expect(held.revision, 'precondition: the home holds revision 1').toBe(1);
    expect(held.material_sha256, 'precondition: the home holds the key minted on it').toBe(
      sha256Hex(KEY3),
    );
    expect(dbCredential(B_DB, acc2), 'precondition: the satellite holds nothing').toBeNull();

    // The operator pastes a DIFFERENT key on the satellite. The node gate has
    // to be satisfied first — a paste homes the account here, and a node the
    // fleet refuses to let hold the account may not become its home — and the
    // switch is the operator's own decision on that node.
    //
    // Nothing pushes the home's material to it in the meantime: the fan-out runs
    // on a local credential WRITE, and the home has not written one since the
    // satellite joined the fleet.
    await setLocalReceives(pageB, true, { peerDb: A_DB, peerNodeId: bId });
    await pasteKey(pageB, ACC2, KEY5);
    const minted = await poll(async () => dbCredential(B_DB, acc2), {
      timeoutMs: 30_000,
      what: 'node B to store the pasted key',
    });
    expect(minted.revision, 'the satellite mints its own revision 1').toBe(1);
    expect(minted.material_sha256).toBe(sha256Hex(KEY5));
    expect(dbAccount(B_DB, acc2)?.home_node_id, 'the paste claims the home').toBe(bId);

    // The frame that write originates reaches the node that already holds
    // revision 1 — which refuses it and says so, in the documented audit.
    const conflict = await poll(async () => auditDetails(A_DB, 'provider_account.credential_conflict', acc2).at(-1) ?? null, {
      timeoutMs: 120_000,
      what: 'the home to refuse the conflicting revision',
    });
    expect(conflict.revision, 'the refused submission offered the same revision').toBe(1);
    expect(conflict.kept_fingerprint, 'the kept copy is named').toBeTruthy();
    expect(conflict.rejected_fingerprint, 'the rejected copy is named').toBeTruthy();
    expect(conflict.kept_fingerprint, 'the two copies differ').not.toBe(conflict.rejected_fingerprint);

    // The material that stands is the one the first writer stored, and the
    // account is locked for the operator rather than silently rewritten.
    const kept = dbCredential(A_DB, acc2);
    expect(kept.revision, 'nothing was overwritten').toBe(1);
    expect(kept.material_sha256, `node A kept the material it already had (${shortSha(kept.material_sha256)})`).toBe(
      sha256Hex(KEY3),
    );
    // `set_credential`'s conflict branch also moves the status to `needs_login`
    // and captures the row, but `status` and `home_node_id` are SYNCED columns
    // and the loser of a conflict has already written its own copy of them: the
    // peer's capture can legitimately land here after the conflict and supersede
    // the flag. The conflict's own node-local record is the audit row above and
    // the untouched material below, so the status is reported rather than
    // asserted.
    console.log(
      `conflict on the home: account status is now ${dbAccount(A_DB, acc2)?.status}, ` +
        `home_node_id is ${dbAccount(A_DB, acc2)?.home_node_id}`,
    );

    // The switch stays ON, and that is the state the rest of the suite runs in:
    // the paste above made this node the HOME of the account, and the store
    // refuses to take a home node out of the fleet (test 6). The one test that
    // needs it outside the fleet is the receiver's refusal, and it ran before
    // this paste.

    await pageA.close();
    await pageB.close();
  });

  // ---------------------------------------------------------------------------
  // 5) The CAS, older revision: a submission below what the home holds cannot
  //    overwrite it, and the refusal records both numbers.
  // ---------------------------------------------------------------------------
  test('an older-revision submission is refused and the home keeps its material', async ({ browser }) => {
    test.setTimeout(300_000);
    const pageA = await openPage(browser, A_PORT);
    const pageB = await openPage(browser, B_PORT);

    // This test needs the home's writes for the third account to stay its OWN,
    // and the node that pastes at the end of it to be a fleet member. The
    // operator's switch is ON — the conflict test left it on, and a node that is
    // some account's home can no longer be taken out of the fleet — so the
    // narrow condition is built the way the receiver gate builds its trigger: a
    // raw write into node A's REPLICA of that flag, removed again before the
    // satellite pastes. Nothing on node B changes: its own row still says it
    // receives accounts, so the paste below is an ordinary operator paste on a
    // fleet member and the home simply was never told to send it anything.
    seed(A_DB, `DELETE FROM agent_runtime_nodes WHERE node_id = ${quote(bId)}`);
    expect(
      receivesAccounts(A_DB, bId),
      'the injected replica says the fleet does not include node B',
    ).toBe(false);

    await openAccountsTab(pageA);
    await createAccountThroughUi(pageA, { engine: ENGINE, name: ACC3, key: KEY6 });
    const acc3 = dbAccountByName(A_DB, ACC3)?.account_id ?? '';
    expect(acc3, 'the third account is not in node A\'s database').toBeTruthy();
    await pasteKey(pageA, ACC3, KEY7);
    const held = dbCredential(A_DB, acc3);
    expect(held.revision, 'the home minted twice').toBe(2);
    expect(held.material_sha256).toBe(sha256Hex(KEY7));

    await poll(async () => dbAccountByName(B_DB, ACC3), {
      timeoutMs: 120_000,
      what: 'the third account row to reach node B',
    });

    // Back in the replica before the paste — node B's own row never changed, so
    // this only makes node A's copy describe the fleet as it really is. A switch
    // flipped back starts no push of its own: the home's writes for the third
    // account happened while the replica excluded this node, for itself alone.
    seed(
      A_DB,
      `INSERT INTO agent_runtime_nodes (node_id, receives_accounts) VALUES (${quote(bId)}, 1)`,
    );
    expect(receivesAccounts(A_DB, bId), 'the replica is back in the fleet').toBe(true);
    expect(dbCredential(B_DB, acc3), 'nothing was fanned out to the satellite').toBeNull();

    // So its own mint starts at 1 — below the home's 2.
    await pasteKey(pageB, ACC3, KEY8);
    const satellite = await poll(async () => dbCredential(B_DB, acc3), {
      timeoutMs: 30_000,
      what: 'node B to store the pasted key',
    });
    expect(satellite.revision).toBe(1);

    const refused = await poll(async () => auditDetails(A_DB, 'provider_account.credential_refused', acc3).at(-1) ?? null, {
      timeoutMs: 120_000,
      what: 'the home to refuse the older revision',
    });
    expect(refused.peer_node_id, 'the refusal names the offering node').toBe(bId);
    expect(refused.reason).toBe('stale_revision');
    expect(refused.revision, 'the offered revision is recorded').toBe(1);
    expect(refused.held_revision, 'the held revision is recorded').toBe(2);

    // "Materiał wygrywa ten, który node macierzysty już ma" — the home is not
    // rewritten by an older submission.
    const kept = dbCredential(A_DB, acc3);
    expect(kept.revision).toBe(2);
    expect(kept.material_sha256, 'the home kept its own material').toBe(sha256Hex(KEY7));

    await pageA.close();
    await pageB.close();
  });

  // ---------------------------------------------------------------------------
  // 3) The home role moves by RE-PLACING the credential.
  // ---------------------------------------------------------------------------
  test('re-placing the same key on another node moves the home without a revision bump', async ({ browser }) => {
    test.setTimeout(300_000);
    const pageA = await openPage(browser, A_PORT);
    const pageB = await openPage(browser, B_PORT);

    const acc1Id = acc1;
    expect(dbAccount(A_DB, acc1Id)?.home_node_id, 'precondition: A is the home').toBe(aId);

    // B takes the fleet flag back on — the operator's own decision, before the
    // credential is placed there.
    await setLocalReceives(pageB, true, { peerDb: A_DB, peerNodeId: bId });

    // The home rotates once more, so the satellite holds material it can also
    // paste — the operator's key and the fleet's key are then the same string.
    const beforeRev = dbCredential(A_DB, acc1).revision;
    await pasteKey(pageA, ACC1, KEY10);
    const homeRev = dbCredential(A_DB, acc1).revision;
    expect(homeRev, 'the home minted one more revision').toBe(beforeRev + 1);

    const adopted = await poll(async () => {
      const credential = dbCredential(B_DB, acc1);
      return credential && credential.revision === homeRev ? credential : null;
    }, { timeoutMs: 120_000, what: 'the satellite to adopt the rotated credential' });
    expect(adopted.material_sha256).toBe(sha256Hex(KEY10));

    // The operator pastes the SAME key on the target node: no new material, so
    // no new revision — and the account becomes home here.
    //
    // The frame above and the account row travel on two different channels (the
    // mesh frame carries the sealed material, the ledger carries `home_node_id`),
    // so the ledger's copy of the rotation is given a moment to land before the
    // paste: the paste is the LAST writer on this node, and a ledger write
    // arriving after it would put the old home back on the row this test is
    // about to read.
    await pageB.waitForTimeout(3000);
    await pasteKey(pageB, ACC1, KEY10);

    const placed = dbCredential(B_DB, acc1);
    expect(placed.revision, 'the same material does not bump the revision').toBe(homeRev);
    expect(placed.material_sha256, 'the satellite kept the same material').toBe(sha256Hex(KEY10));
    expect(dbAccount(B_DB, acc1)?.home_node_id, 'node B is the home now').toBe(bId);

    // The move is recorded on the node that took the role, naming the node it
    // took it from.
    const moved = await poll(async () => auditDetails(B_DB, 'provider_account.home_moved', acc1).at(-1) ?? null, {
      timeoutMs: 60_000,
      what: 'the home move to be audited',
    });
    expect(moved.previous_home).toBe(aId);
    expect(moved.home).toBe(bId);

    // The old home stops being it — through the ledger, in its own database.
    await poll(async () => (dbAccount(A_DB, acc1)?.home_node_id === bId ? true : null), {
      timeoutMs: 120_000,
      what: 'node A to learn it is no longer the home',
    });
    const old = dbCredential(A_DB, acc1);
    expect(old.revision, "the old home's copy is untouched").toBe(homeRev);
    expect(old.material_sha256).toBe(sha256Hex(KEY10));

    // And on the wire, from BOTH nodes: the home is B.
    const wireB = await accountAfterReload(pageB, acc1);
    expect(wireB.home_node_id).toBe(bId);
    const wireA = await accountAfterReload(pageA, acc1);
    expect(wireA.home_node_id).toBe(bId);

    await pageA.close();
    await pageB.close();
  });

  // ---------------------------------------------------------------------------
  // 6) The operator's own control on a node that is a home: the switch is
  //    REFUSED, with the remedy its accounts actually have.
  // ---------------------------------------------------------------------------
  test('taking a home node out of the fleet is refused with the remedy for its accounts', async ({ browser }) => {
    test.setTimeout(180_000);
    const pageB = await openPage(browser, B_PORT);

    // Which accounts call this node home is stated by node B's OWN rows rather
    // than assumed: the conflict test ends with two nodes claiming the same
    // account (`home_node_id` is a SYNCED column, so the last ledger write
    // decides), and the refusal below has to name the number this node actually
    // counted, not the number this suite created.
    const homed = [ACC1, ACC2, ACC3]
      .map((name) => dbAccountByName(B_DB, name))
      .filter((account) => account && account.home_node_id === bId);
    expect(homed.length, 'precondition: node B is the home of at least one account').toBeGreaterThan(0);
    expect(
      homed.every((account) => account.credential_kind === 'api_key'),
      'precondition: every account homed here took a pasted key',
    ).toBe(true);

    const before = await localRow(pageB);
    expect(before.receives_accounts, 'precondition: the switch is on').toBe(true);
    const decisionsBefore = auditDetails(B_DB, 'agent_runtime.receives_accounts', bId).length;

    await openRuntimeSegment(pageB);
    await toggleLocalReceives(pageB);

    // The refusal is shown in the operator's own language, and the switch
    // bounces back instead of reading as a decision that was taken.
    const toast = await poll(
      async () => {
        const texts = await pageB.locator('.toast.toast-error').allTextContents();
        return texts.find((text) => text.includes('Na tym nodzie zalogowano konta agentów')) ?? null;
      },
      { timeoutMs: 30_000, what: 'the refusal toast' },
    );
    expect(toast).toContain('Na tym nodzie zalogowano konta agentów');
    expect(toast, 'the node\'s English sentence reached the toast').not.toContain('is the home of');
    expect(await localToggleChecked(pageB), 'the switch did not move').toBe(true);
    expect(receivesAccounts(B_DB, bId), 'the flag was not written').toBe(true);
    // Counted, not enumerated: the fleet's switches are flipped by several tests
    // in this suite, and what a REFUSED one must not do is add a row.
    expect(
      auditDetails(B_DB, 'agent_runtime.receives_accounts', bId).length,
      'a refused change leaves no audit row claiming it happened',
    ).toBe(decisionsBefore);

    // The refusal on the wire names the count and the remedy THIS kind of
    // account has: a paste, never a provider sign-in.
    const refusal = await apiError(pageB, 'providerAccountRuntimeSetReceivesAccountsRequest', {
      nodeId: bId,
      enabled: false,
    });
    expect(refusal, 'the refusal never reached the wire').not.toBeNull();
    expect(refusal.code).toBe('BadRequest');
    expect(refusal.message).toContain(`is the home of ${homed.length} agent account(s)`);
    expect(refusal.message).toContain('paste their API key');
    expect(refusal.message).not.toContain('sign those accounts in');
    expect(receivesAccounts(B_DB, bId), 'the refused wire call wrote nothing either').toBe(true);

    await pageB.close();
  });
});
