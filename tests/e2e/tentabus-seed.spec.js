// =============================================================================
// File: tests/e2e/tentabus-seed.spec.js
// Description: Minimal end-to-end smoke test for the TentaBus multi-instance
//              demo seeder (tentaflow-core/tests/bus_demo_seed.rs). Boots an
//              isolated tentaflow instance (own port, sqlite db and
//              TENTAFLOW_HOME), seeds TWO real TentaBus instances offline via
//              `cargo test --test bus_demo_seed -- --ignored seed_demo_data`
//              (same two-phase "boot once to migrate, seed offline, boot
//              again" pattern as tests/e2e/analytics.spec.js and
//              tests/e2e/tentaquant.spec.js), then drives the dashboard as an
//              admin would: the instance gate must show both seeded
//              instances, and entering one must show its seeded topics.
// =============================================================================

const { test, expect } = require('@playwright/test');
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');

const PORT = 18322;
const WORK_DIR = path.join(os.tmpdir(), `tentaflow-e2e-tentabus-seed-${PORT}`);
const DB = path.join(WORK_DIR, 'tentabus-seed.db');
const HOME = path.join(WORK_DIR, 'home');
const WWW_DIR = path.join(__dirname, '../../tentaflow-core/www');
const REPO_ROOT = path.join(__dirname, '../..');

// Must match tentaflow-core/tests/bus_demo_seed.rs's INSTANCE_SPECS.
const INSTANCE_NAMES = ['seed-primary', 'seed-secondary'];
const LAB_TOPIC = 'lab.results';
const ORDERS_TOPIC = 'orders.created';

let server = null;
// Set when the seeder's migration ladder turns out to be ahead of the spawned
// binary's — see `migrationHead`'s doc below. Every test then skips with that
// exact reason instead of failing as an unexplained boot timeout.
let staleBinaryReason = null;

// Highest applied migration in `db`. The seeder is compiled from the CURRENT
// tree while `startBinary` runs whatever binary was built last, so the two can
// disagree about how far the ladder goes. When the seeder migrates past the
// binary's head, `db::init` REFUSES the database on the next boot ("newer than
// this build's own migration ladder") and the server simply never answers —
// which surfaces as a bare 60 s timeout unless the mismatch is named. Reading
// the head before and after the seeder turns that into an exact diagnosis.
function migrationHead(dbPath) {
  const out = execFileSync('/usr/bin/sqlite3', [dbPath, 'SELECT MAX(version) FROM _migrations;'], {
    encoding: 'utf8',
  });
  return Number(out.trim());
}

function runSeeder(testName) {
  execFileSync(
    'cargo',
    ['test', '-p', 'tentaflow-core', '--test', 'bus_demo_seed', '--', '--ignored', testName, '--nocapture'],
    {
      cwd: REPO_ROOT,
      env: {
        ...process.env,
        TENTABUS_SEED_DB: DB,
        TENTABUS_SEED_HOME: HOME,
      },
      stdio: 'inherit',
      timeout: 900000,
    },
  );
}

async function stopAndWait(proc) {
  if (!proc) return;
  const exited = new Promise((resolve) => proc.once('exit', resolve));
  stopBinary(proc);
  await Promise.race([exited, new Promise((r) => setTimeout(r, 10000))]);
}

test.beforeAll(async () => {
  // Two boots plus the seeder, which cargo may have to compile first.
  test.setTimeout(900000);
  test.skip(!binaryExists(), 'tentaflow binary not built (target_shared/{release-fast,release,debug})');
  fs.rmSync(WORK_DIR, { recursive: true, force: true });
  fs.mkdirSync(WORK_DIR, { recursive: true });

  // Phase 1: boot once to create + migrate the main db and native package
  // catalog, then stop — the seeder must run against a STOPPED app (flock).
  const first = startBinary({ port: PORT, db: DB, home: HOME, env: { TENTAFLOW_WWW_DIR: WWW_DIR } });
  await waitForServer(PORT, 60000);
  await stopAndWait(first);
  const binaryHead = migrationHead(DB);

  // Phase 2: seed two real instances offline.
  runSeeder('seed_demo_data');
  const seederHead = migrationHead(DB);
  if (seederHead > binaryHead) {
    staleBinaryReason =
      `tentaflow binary is older than this source tree: it migrates to ${binaryHead}, the seeder ` +
      `to ${seederHead}, and db::init refuses a database newer than the binary's own ladder. ` +
      'Rebuild the binary from this tree (./scripts/build.sh --profile release-fast) and re-run.';
    return;
  }

  // Phase 3: boot again on the seeded state for the actual UI drive. The two
  // seeded instances are `is_enabled = true`, so the boot-time native-
  // instance pass starts their engines against the state the seeder wrote.
  server = startBinary({ port: PORT, db: DB, home: HOME, env: { TENTAFLOW_WWW_DIR: WWW_DIR }, keepDb: true });
  await waitForServer(PORT, 60000);
});

// A stale binary is a build-freshness problem, not a product defect — the suite
// says so and skips, the same way it already skips when no binary exists at all.
test.beforeEach(() => {
  test.skip(staleBinaryReason !== null, staleBinaryReason ?? '');
});

test.afterAll(async () => {
  await stopAndWait(server);
  server = null;
});

function trackErrors(page) {
  const errors = [];
  page.on('console', (m) => { if (m.type() === 'error') errors.push(`[console] ${m.text().slice(0, 400)}`); });
  page.on('pageerror', (e) => errors.push(`[pageerror] ${e.message.slice(0, 400)}`));
  return errors;
}

async function open(page) {
  await page.addInitScript(() => {
    document.addEventListener('DOMContentLoaded', () => {
      const st = document.createElement('style');
      st.textContent = '.update-overlay{display:none!important}';
      document.head.appendChild(st);
    });
  });
  await loginAsAdmin(page, { port: PORT });
}

test('TentaBus instance gate shows both seeded instances', async ({ page }) => {
  const errors = trackErrors(page);
  await open(page);
  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus`);

  const rows = page.locator('.tb-instance-row');
  await expect(rows).toHaveCount(INSTANCE_NAMES.length);

  const titles = await rows.locator('.tb-instance-row-title').allTextContents();
  for (const name of INSTANCE_NAMES) {
    expect(titles.some((t) => t.trim() === name)).toBeTruthy();
  }

  expect(errors, `console/page errors: ${errors.join('\n')}`).toEqual([]);
});

for (const name of INSTANCE_NAMES) {
  test(`TentaBus '${name}' instance shows its seeded topics`, async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await page.goto(`https://127.0.0.1:${PORT}/#/tentabus`);

    await page.locator('.tb-instance-row', { hasText: name }).click();
    await expect(page.locator('#tb-tabs')).toBeVisible();
    // The instance opens on Przegląd; the topic list is its own tab.
    await page.locator('#tb-tabs tf-tab#topics > button').click();

    const table = page.locator('#tb-topics-table');
    // The broker's own `__*` topics (each topic's unprocessed-message store,
    // metrics) are not topics of the reader's: the seeded pair is two rows.
    await expect(table.locator('tbody tr')).toHaveCount(2, { timeout: 15000 });
    // tf-table renders its rows in a shadow root, so the host's own innerText
    // is empty — the text has to come from the row locators themselves.
    const rowText = (await table.locator('tbody tr').allTextContents()).join('\n');
    expect(rowText).toContain(LAB_TOPIC);
    expect(rowText).toContain(ORDERS_TOPIC);
    expect(rowText).not.toContain('__');

    expect(errors, `console/page errors: ${errors.join('\n')}`).toEqual([]);
  });
}
