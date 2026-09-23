// =============================================================================
// File: tests/e2e/access-keys-bus-schema.spec.js
// Description: "Wzory wiadomości" scope of a general API key, end to end:
//              the admin issues a WRITE-only schema-registry key in the real
//              key wizard, the schema registry REST (`/v1/bus/instances/{id}/
//              schemas/...`) accepts a registration with it and refuses a read
//              (write never implies read), then the admin adds READ in the
//              Per klucz matrix and the same key can read. Boots its own node
//              and seeds two real TentaBus instances offline with the same
//              two-phase pattern as tentabus-seed.spec.js. The runtime lives
//              under ~/.cache, not /tmp (macOS /tmp is a symlink and has
//              broken node rigs before).
// =============================================================================

const { test, expect } = require('@playwright/test');
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');

const PORT = 18333;
const WORK_DIR = path.join(os.homedir(), '.cache', 'tentaflow-e2e', `access-keys-bus-schema-${PORT}`);
const DB = path.join(WORK_DIR, 'access-keys-bus-schema.db');
const HOME = path.join(WORK_DIR, 'home');
const WWW_DIR = path.join(__dirname, '../../tentaflow-core/www');
const REPO_ROOT = path.join(__dirname, '../..');
const BASE = `https://127.0.0.1:${PORT}`;

// Seeded by tentaflow-core/tests/bus_demo_seed.rs; the org is the migration's
// `org-default` row.
const INSTANCE_TITLE = 'seed-primary';
const ORG_NAME = 'Default Organization';
const SCHEMA = '{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}';

let server = null;
let staleBinaryReason = null;

function sqlite(sql) {
  return execFileSync('/usr/bin/sqlite3', [DB, sql], { encoding: 'utf8' }).trim();
}

function runSeeder() {
  execFileSync(
    'cargo',
    ['test', '-p', 'tentaflow-core', '--test', 'bus_demo_seed', '--', '--ignored', 'seed_demo_data', '--nocapture'],
    {
      cwd: REPO_ROOT,
      env: { ...process.env, TENTABUS_SEED_DB: DB, TENTABUS_SEED_HOME: HOME },
      stdio: 'inherit',
      timeout: 600000,
    },
  );
}

async function stopAndWait(proc) {
  if (!proc) return;
  const exited = new Promise((resolve) => proc.once('exit', resolve));
  stopBinary(proc);
  await Promise.race([exited, new Promise((r) => setTimeout(r, 10000))]);
}

// Inverse of `sync::resource_id::composite_resource_id`, byte-length prefixed.
function decodeComposite(id) {
  const bytes = Buffer.from(id, 'utf8');
  const parts = [];
  let pos = 0;
  while (pos < bytes.length) {
    const sep = bytes.indexOf(0x1f, pos);
    const len = Number(bytes.subarray(pos, sep).toString('utf8'));
    parts.push(bytes.subarray(sep + 1, sep + 1 + len).toString('utf8'));
    pos = sep + 1 + len;
  }
  return parts;
}

test.beforeAll(async () => {
  // Two node boots plus a seeder that `cargo test` may still have to compile:
  // far past the 120 s a single test gets, and a hook timeout here would read
  // like a broken screen.
  test.setTimeout(900_000);
  test.skip(!binaryExists(), 'tentaflow binary not built (target_shared/{release-fast,release,debug})');
  fs.rmSync(WORK_DIR, { recursive: true, force: true });
  fs.mkdirSync(WORK_DIR, { recursive: true });

  const first = startBinary({ port: PORT, db: DB, home: HOME, env: { TENTAFLOW_WWW_DIR: WWW_DIR } });
  await waitForServer(PORT, 60000);
  await stopAndWait(first);
  const binaryHead = Number(sqlite('SELECT MAX(version) FROM _migrations;'));

  runSeeder();
  const seederHead = Number(sqlite('SELECT MAX(version) FROM _migrations;'));
  if (seederHead > binaryHead) {
    staleBinaryReason =
      `tentaflow binary migrates to ${binaryHead}, the seeder to ${seederHead}; ` +
      'rebuild the binary from this tree (./scripts/build.sh --profile release-fast) and re-run.';
    return;
  }

  server = startBinary({ port: PORT, db: DB, home: HOME, env: { TENTAFLOW_WWW_DIR: WWW_DIR }, keepDb: true });
  await waitForServer(PORT, 60000);
});

test.beforeEach(() => {
  test.skip(staleBinaryReason !== null, staleBinaryReason ?? '');
});

test.afterAll(async () => {
  await stopAndWait(server);
  server = null;
});

test('write-only Wzory wiadomości key registers but cannot read until read is granted', async ({ page, request }) => {
  const errors = [];
  page.on('pageerror', (e) => errors.push(`[pageerror] ${e.message.slice(0, 400)}`));

  await loginAsAdmin(page, { port: PORT });
  await page.goto(`${BASE}/#/access-keys`);

  // ---- Wizard: general key with a write-only schema registry grant -------
  await page.locator('#ak-create').click();
  const win = page.locator('tf-window').last();
  await win.locator('.ak-type-card[data-type="general"]').click();
  await win.locator('#ak-name input').fill('schema-producer');
  await win.locator('#ak-next').click();

  await expect(win.locator('.ak-bus-section')).toBeVisible();
  // A half-made pick blocks Create instead of silently vanishing.
  await win.locator('#ak-bus-instance select').selectOption({ label: INSTANCE_TITLE });
  await win.locator('#ak-create-btn').click();
  await expect(page.locator('.toast, tf-toast, .tf-toast').last()).toBeVisible();
  await expect(win.locator('#ak-token')).toHaveCount(0);
  expect(sqlite("SELECT COUNT(*) FROM api_keys WHERE name = 'schema-producer';")).toBe('0');

  // A complete pick left in the pickers (no "Dodaj") is taken on Create.
  await win.locator('#ak-bus-org select').selectOption({ label: ORG_NAME });
  await win.locator('#ak-bus-write').click();
  await expect(win.locator('#ak-bus-add')).toHaveAttribute('variant', 'secondary');
  await win.locator('#ak-create-btn').click();
  const token = (await win.locator('#ak-token').textContent()).trim();
  expect(token).toMatch(/^sk-[0-9a-f]{64}$/);
  await win.locator('#ak-done').click();

  // The stored grant is exactly one WRITE row — no read, no action-blind '*'.
  const rows = sqlite(
    // hex(): the sqlite3 shell escapes the U+001F separator on output.
    "SELECT action || '|' || access_level || '|' || hex(resource_id) FROM resource_permissions " +
      "WHERE resource_type = 'bus_schema_registry';",
  ).split('\n').filter(Boolean);
  expect(rows).toHaveLength(1);
  const [action, level, scopeHex] = rows[0].split('|');
  const scopeId = Buffer.from(scopeHex, 'hex').toString('utf8');
  expect(action).toBe('write');
  expect(level).toBe('allow');
  const [instanceId, orgId] = decodeComposite(scopeId);
  expect(instanceId).toMatch(/^tentabus-[0-9a-f]{8}$/);
  expect(orgId).toBe('org-default');

  // ---- REST: write works, read is refused ---------------------------------
  const auth = { Authorization: `Bearer ${token}` };
  const subjects = `${BASE}/v1/bus/instances/${instanceId}/schemas/subjects`;
  const registered = await request.post(`${subjects}/orders.created/versions?org_id=${orgId}`, {
    headers: auth,
    data: { schema_type: 'json_schema', schema_text: SCHEMA },
  });
  expect(registered.status(), await registered.text()).toBe(201);
  expect((await registered.json()).version).toBe(1);

  const deniedRead = await request.get(`${subjects}?org_id=${orgId}`, { headers: auth });
  expect(deniedRead.status()).toBe(403);
  expect((await deniedRead.json()).error.type).toBe('permission_error');

  // ---- Matrix: grant READ on the same scope in Per klucz -----------------
  await page.locator('#ak-tabs .tf-tab-btn[data-tab="matrix"]').click();
  await page.locator('#ak-msub .subtab[data-sub="api_key"]').click();
  await expect(page.locator('.perm-matrix th.grp', { hasText: /Wzory wiadomości|Message schemas/ })).toBeVisible();
  // U+001F separates the composite id's parts; spell it as a CSS escape.
  const ridSelector = scopeId.replace(/\x1f/g, '\\1f ');
  const readCell = page.locator(
    `.perm-btn[data-rtype="bus_schema_registry"][data-action="read"][data-rid="${ridSelector}"]`,
  );
  await expect(readCell).toHaveAttribute('data-mode', 'inherit');
  const writeCell = page.locator(
    `.perm-btn[data-rtype="bus_schema_registry"][data-action="write"][data-rid="${ridSelector}"]`,
  );
  await expect(writeCell).toHaveAttribute('data-mode', 'allow');
  await readCell.click();
  await expect(readCell).toHaveAttribute('data-mode', 'allow');
  await expect
    .poll(() => sqlite(
      "SELECT COUNT(*) FROM resource_permissions WHERE resource_type = 'bus_schema_registry' " +
        "AND action = 'read' AND access_level = 'allow';",
    ))
    .toBe('1');

  const read = await request.get(`${subjects}?org_id=${orgId}`, { headers: auth });
  expect(read.status(), await read.text()).toBe(200);
  const body = await read.json();
  expect(body.subjects.map((s) => s.subject)).toEqual(['orders.created']);

  // The key-name column stays pinned while the wide grid scrolls sideways.
  await page.evaluate(() => { const g = document.getElementById('ak-matrix'); g.scrollLeft = g.scrollWidth; });
  const gridLeft = await page.locator('#ak-matrix').evaluate((e) => Math.round(e.getBoundingClientRect().left));
  const subjectCell = page.locator('.perm-matrix tbody td.ak-subject-col').first();
  const cellLeft = await subjectCell.evaluate((e) => Math.round(e.getBoundingClientRect().left));
  expect(Math.abs(cellLeft - gridLeft)).toBeLessThanOrEqual(2);
  await expect(subjectCell).toHaveCSS('position', 'sticky');
  await expect(subjectCell).toBeVisible();

  // The column headers name the instance and organisation, never their ids.
  const headers = await page.locator('.perm-matrix th.func').allTextContents();
  expect(headers.join('\n')).not.toContain('tentabus-');

  expect(errors, errors.join('\n')).toEqual([]);
});
