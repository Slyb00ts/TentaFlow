// =============================================================================
// File: tests/e2e/tentabus-ui.spec.js
// Description: TentaBus screen shell, Przegląd (PLAN-UI-20260923 U0) and
//              Topiki with the topic creator, delete and preview (U1) on a
//              real node seeded with the "Przychodnia Zdrowie" world
//              (`seed_clinic_data` in tentaflow-core/tests/bus_demo_seed.rs):
//              boot once to migrate, seed offline, boot again. Drives T01 at
//              1440x900 and 390x844, the six main tabs, the alerts' buttons,
//              reload of a tab and of a topic, and switching to the empty
//              instance (T11); then T02/T04: the topic list, its filters and
//              footer, the creator (validation, three steps, the new row),
//              the message preview, delete with the retyped name, the phone
//              layout, the empty instance's creator and, last because it
//              stops the node, the list kept under the connection notice (T12). Stateful: run the whole
//              project, never `-g`.
//              The runtime lives under the repo's `.runtime/` — on macOS a
//              rig under /tmp (a symlink to /private/tmp) is not reliable.
// =============================================================================

const { test, expect } = require('@playwright/test');
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');

const PORT = 18323;
const REPO_ROOT = path.join(__dirname, '../..');
const WORK_DIR = path.join(REPO_ROOT, '.runtime', `e2e-tentabus-ui-${PORT}`);
const DB = path.join(WORK_DIR, 'tentabus-ui.db');
const HOME = path.join(WORK_DIR, 'home');
const WWW_DIR = path.join(REPO_ROOT, 'tentaflow-core/www');
const SHOTS = path.join(WORK_DIR, 'shots');

const DESKTOP = { width: 1440, height: 900 };
const PHONE = { width: 390, height: 844 };

let server = null;
let staleBinaryReason = null;

function migrationHead(dbPath) {
  return Number(execFileSync('/usr/bin/sqlite3', [dbPath, 'SELECT MAX(version) FROM _migrations;'], { encoding: 'utf8' }).trim());
}

async function stopAndWait(proc) {
  if (!proc) return;
  const exited = new Promise((resolve) => proc.once('exit', resolve));
  stopBinary(proc);
  await Promise.race([exited, new Promise((r) => setTimeout(r, 10000))]);
}

test.describe.configure({ mode: 'serial' });
// The mockups and every assertion below are Polish.
test.use({ locale: 'pl-PL' });

test.beforeAll(async () => {
  // Two boots plus the seeder, which cargo may have to compile first.
  test.setTimeout(900000);
  test.skip(!binaryExists(), 'tentaflow binary not built (target_shared/{release-fast,release,debug})');
  fs.rmSync(WORK_DIR, { recursive: true, force: true });
  fs.mkdirSync(SHOTS, { recursive: true });

  const first = startBinary({ port: PORT, db: DB, home: HOME, env: { TENTAFLOW_WWW_DIR: WWW_DIR } });
  await waitForServer(PORT, 60000);
  await stopAndWait(first);
  const binaryHead = migrationHead(DB);

  execFileSync('cargo', ['test', '-p', 'tentaflow-core', '--test', 'bus_demo_seed', '--', '--ignored', 'seed_clinic_data', '--nocapture'], {
    cwd: REPO_ROOT,
    env: { ...process.env, TENTABUS_SEED_DB: DB, TENTABUS_SEED_HOME: HOME },
    stdio: 'inherit',
    timeout: 900000,
  });
  const seederHead = migrationHead(DB);
  if (seederHead > binaryHead) {
    staleBinaryReason = `tentaflow binary migrates to ${binaryHead}, the seeder to ${seederHead}; rebuild the binary from this tree.`;
    return;
  }

  const bootAt = Date.now();
  server = startBinary({ port: PORT, db: DB, home: HOME, env: { TENTAFLOW_WWW_DIR: WWW_DIR }, keepDb: true });
  await waitForServer(PORT, 60000);
  // The screen is checked only after the server's own sampler has added a
  // live sample to the seeded lag history: the "nie nadąża" alert must hold
  // on what the node measures, not only on what was seeded.
  await waitForLiveLagSample(bootAt, 180000);
});

function laggedSampleDbs() {
  const root = path.join(HOME, 'orgs');
  const out = [];
  for (const org of fs.readdirSync(root)) {
    const addons = path.join(root, org, 'addons');
    if (!fs.existsSync(addons)) continue;
    for (const a of fs.readdirSync(addons)) {
      const db = path.join(addons, a, 'tentabus.db');
      if (fs.existsSync(db)) out.push(db);
    }
  }
  return out;
}

async function waitForLiveLagSample(sinceMs, maxWaitMs) {
  const deadline = Date.now() + maxWaitMs;
  while (Date.now() < deadline) {
    for (const db of laggedSampleDbs()) {
      const newest = Number(execFileSync('/usr/bin/sqlite3', [db, "SELECT COALESCE(MAX(sampled_at_ms), 0) FROM bus_lag_samples WHERE group_id = 'aplikacja-lekarza';"], { encoding: 'utf8' }).trim());
      if (newest > sinceMs) return;
    }
    await new Promise((r) => setTimeout(r, 2000));
  }
  throw new Error('the server took no lag sample of its own');
}

test.beforeEach(() => {
  test.skip(staleBinaryReason !== null, staleBinaryReason ?? '');
});

test.afterAll(async () => {
  await stopAndWait(server);
  server = null;
});

// The service worker cannot register over the test node's self-signed
// certificate; that is the rig, not the screen.
const RIG_NOISE = /An SSL certificate error occurred when fetching the script/;

function trackErrors(page) {
  const errors = [];
  page.on('console', (m) => { if (m.type() === 'error' && !RIG_NOISE.test(m.text())) errors.push(`[console] ${m.text().slice(0, 300)}`); });
  page.on('pageerror', (e) => errors.push(`[pageerror] ${e.message.slice(0, 300)}`));
  return errors;
}

async function login(page) {
  await page.addInitScript(() => {
    localStorage.setItem('tentaflow_lang', 'pl');
    document.addEventListener('DOMContentLoaded', () => {
      const st = document.createElement('style');
      st.textContent = '.update-overlay{display:none!important}';
      document.head.appendChild(st);
    });
  });
  await loginAsAdmin(page, { port: PORT });
}

// Enters an instance through the gate and returns its id from the address.
async function openInstance(page, name) {
  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus`);
  await page.locator('.tb-instance-row', { hasText: name }).click();
  await expect(page.locator('#tb-tabs')).toBeVisible();
  await expect(page).toHaveURL(/instance=tentabus-/);
  return new URL(page.url()).hash.match(/instance=([^&]+)/)[1];
}

const tab = (page, id) => page.locator(`#tb-tabs tf-tab#${id} > button`);
const overview = (page) => page.locator('#tb-panel > [data-tb-view-slot="overview"]');
const topicsSlot = (page) => page.locator('#tb-panel > [data-tb-view-slot="topics"]');
const hashParams = (page) => Object.fromEntries(new URLSearchParams(new URL(page.url()).hash.split('?')[1] || ''));
const norm = (s) => String(s).replace(/[\u00a0\u202f]/g, ' ').replace(/\s+/g, ' ').trim();

// No horizontal page scroll, and nothing inside the screen root wider than it.
async function assertNoOverflow(page) {
  const problems = await page.evaluate(() => {
    const out = [];
    const doc = document.scrollingElement;
    if (doc.scrollWidth > doc.clientWidth + 1) out.push(`page scrolls horizontally: ${doc.scrollWidth} > ${doc.clientWidth}`);
    const root = document.querySelector('#tb-root');
    const r = root.getBoundingClientRect();
    for (const el of root.querySelectorAll('.tb-app-head, tf-tabs, .section-card, tf-stat-card, .topic-mini, .tb-alert, .job-row, tf-button')) {
      const b = el.getBoundingClientRect();
      if (b.width === 0) continue;
      if (b.left < r.left - 1 || b.right > r.right + 1) out.push(`${el.tagName.toLowerCase()}.${el.className} sticks out: ${Math.round(b.left)}..${Math.round(b.right)} of ${Math.round(r.left)}..${Math.round(r.right)}`);
    }
    return out;
  });
  expect(problems, problems.join('\n')).toEqual([]);
}

// Every text the screen shows, shadow roots and select options included.
async function screenText(page) {
  return page.evaluate(() => {
    const parts = [];
    const walk = (node) => {
      if (node.nodeType === Node.TEXT_NODE) { parts.push(node.textContent); return; }
      if (node.nodeType !== Node.ELEMENT_NODE && node.nodeType !== Node.DOCUMENT_FRAGMENT_NODE) return;
      if (node.nodeType === Node.ELEMENT_NODE) {
        if (getComputedStyle(node).display === 'none') return;
        for (const attr of ['label', 'title', 'value', 'delta', 'suffix', 'message']) {
          if (node.hasAttribute?.(attr) && node.tagName.includes('-')) parts.push(node.getAttribute(attr));
        }
        if (node.shadowRoot) walk(node.shadowRoot);
      }
      for (const c of node.childNodes) walk(c);
    };
    walk(document.querySelector('#tb-root'));
    return parts.join('\n');
  });
}

async function assertNoInternalTopics(page) {
  const text = await screenText(page);
  const hits = text.split(/\s+/).filter((w) => /__\w/.test(w));
  expect(hits, `internal topic names on screen: ${hits.join(', ')}`).toEqual([]);
}

// Every chip on screen — tf-table shadow roots included — reads as words.
async function assertSentenceCaseChips(page) {
  const found = await page.evaluate(() => {
    const out = [];
    const walk = (root) => {
      for (const el of root.querySelectorAll('.tf-chip')) {
        if (el.getBoundingClientRect().width > 0) out.push([getComputedStyle(el).textTransform, el.textContent.trim()]);
      }
      for (const el of root.querySelectorAll('*')) if (el.shadowRoot) walk(el.shadowRoot);
    };
    walk(document.querySelector('#tb-root'));
    return out;
  });
  const upper = found.filter(([t]) => t !== 'none').map(([, text]) => text);
  expect(upper, `upper-cased chips: ${upper.join(', ')}`).toEqual([]);
}

async function assertNoBannedWords(page) {
  const text = await page.locator('#tb-root').innerText();
  expect(text).not.toMatch(/\bDLQ\b/);
  expect(text).not.toMatch(/węz(eł|ła|le|ły|łów)/i);
  expect(text).not.toMatch(/\bNOWE\b|wkrótce/);
}

test('T01 at 1440x900: header, tabs with counters, KPI, busiest topics, alerts, nodes', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openInstance(page, 'Produkcja');
  const ov = overview(page);
  await expect(ov.locator('.tb-alert').first()).toBeVisible({ timeout: 20000 });

  // Header card: running, one node, the instance and how fresh the data is.
  const head = page.locator('.tb-app-head');
  await expect(head.locator('[data-role="status"]')).toHaveAttribute('label', 'Działa');
  await expect(head.locator('[data-role="nodes"]')).toHaveAttribute('label', '1 node', { timeout: 15000 });
  await expect(page.locator('#tb-head-sub')).toContainText('instancja Produkcja');
  await expect(page.locator('#tb-head-sub')).toContainText(/odświeżono \d+ s temu/);
  await expect(head.locator('[data-role="topics"]')).toHaveAttribute('label', '3 topiki');
  await expect(head.locator('[data-role="dlq"]')).toHaveAttribute('label', '14 nieprzetworzonych wiadomości');
  await expect(head.locator('[data-role="schemas"]')).toHaveAttribute('label', '2 wzory wiadomości');
  await expect(page.locator('#tb-instance-select select')).toHaveValue(/tentabus-/);

  // Breadcrumb and the six tabs with their counters.
  await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(['TentaBus', 'Produkcja']);
  const counts = await page.locator('#tb-tabs tf-tab').evaluateAll((tabs) => tabs.map((t) => [t.id, t.getAttribute('count')]));
  expect(counts).toEqual([['overview', null], ['topics', '3'], ['groups', '4'], ['dlq', '14'], ['schemas', '2'], ['replication', '1']]);
  await expect(tab(page, 'overview')).toHaveAttribute('aria-selected', 'true');

  // KPI tiles.
  const tile = (key) => ov.locator(`tf-stat-card[data-kpi="${key}"]`);
  await expect(tile('topics')).toHaveAttribute('value', '3');
  expect(norm(await tile('topics').getAttribute('suffix'))).toBe('· 8 partycji');
  // Bytes of the three topics only, the same figures the topic rows list.
  const rowBytes = (await ov.locator('.topic-mini [data-role="sub"]').allTextContents()).map((t) => norm(t).split(' · ').pop());
  expect(rowBytes).toHaveLength(3);
  await expect(tile('topics')).toHaveAttribute('delta-position', 'under-value');
  await expect(tile('groups')).toHaveAttribute('label', 'Odbiorcy z opóźnieniem');
  await expect(tile('groups')).toHaveAttribute('value', '3');
  expect(norm(await tile('groups').getAttribute('suffix'))).toBe('z 4');
  await expect(tile('groups')).toHaveAttribute('delta', 'aplikacja-lekarza, rejestracja-online, system-rozliczen');
  await expect(tile('dlq')).toHaveAttribute('value', '14');

  // Busiest topics: three rows with the payload kind in words.
  const rows = ov.locator('.topic-mini');
  await expect(rows).toHaveCount(3);
  const subs = (await rows.locator('[data-role="sub"]').allTextContents()).map(norm);
  expect(subs.some((s) => s.startsWith('HL7 v2 · 3 partycje'))).toBeTruthy();
  expect(subs.some((s) => s.startsWith('JSON · 3 partycje'))).toBeTruthy();
  expect(subs.some((s) => s.startsWith('XML · 2 partycje'))).toBeTruthy();

  // Alerts computed from the stats, the lag history and the thresholds.
  const titles = (await ov.locator('.tb-alert [data-role="title"]').allTextContents()).map(norm);
  expect(titles).toEqual([
    'Odbiorca aplikacja-lekarza nie nadąża',
    'Przybywa nieprzetworzonych wiadomości',
    'Odbiorca system-rozliczen jest wstrzymany',
  ]);
  // Nobody consumes or produces during the run: the backlog waits, it does not grow.
  await expect(ov.locator('.tb-alert').first()).toContainText(/czeka od 2\d min/);
  await expect(ov.locator('.tb-alert').nth(1)).toContainText('14 w ostatniej godzinie, razem 14');
  await expect(ov.locator('[data-role="alerts-count"] tf-chip')).toHaveAttribute('label', '3');

  // Replica state of the one node.
  await expect(ov.locator('[data-role="nodes"] .job-row')).toHaveCount(1);
  await expect(ov.locator('[data-role="nodes"] [data-role="state"]')).toHaveAttribute('label', 'działa');
  await expect(ov.locator('[data-role="nodes-sub"]')).toContainText('jeden node');
  // Counted over the three topics' 8 partitions, like the KPI tile.
  await expect(ov.locator('[data-role="nodes"] [data-role="leads"]')).toHaveText('prowadzi 8 partycji');
  await expect(ov.locator('[data-role="nodes"] [data-role="holds"]')).toHaveText('');
  await expect(ov.locator('[data-role="nodes"] [data-role="sync"]')).toHaveAttribute('label', '8 z 8 zgodnych');
  // Chips read as words, not upper-case tags.
  await assertSentenceCaseChips(page);
  // The rate axis counts whole messages.
  const ticks = await ov.locator('tf-stream-chart .tf-chart__axis--y text').allTextContents();
  expect(ticks.length).toBeGreaterThan(1);
  for (const t of ticks) expect(norm(t)).toMatch(/^\d[\d ]*$/);
  expect(new Set(ticks).size).toBe(ticks.length);
  await expect(ov.locator('tf-stream-chart')).toBeVisible();

  await assertNoOverflow(page);
  await assertNoBannedWords(page);
  await assertNoInternalTopics(page);
  await page.screenshot({ path: path.join(SHOTS, 't01-przeglad.png'), fullPage: true });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('T01 at 390x844: one column, no horizontal scroll, the same content', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(PHONE);
  await login(page);
  await openInstance(page, 'Produkcja');
  const ov = overview(page);
  await expect(ov.locator('.tb-alert')).toHaveCount(3, { timeout: 20000 });
  const cols = await ov.locator('.tb-kpi').evaluate((el) => getComputedStyle(el).gridTemplateColumns.split(' ').length);
  expect(cols).toBe(2);
  const dash = await ov.locator('.tb-dash').first().evaluate((el) => getComputedStyle(el).gridTemplateColumns.split(' ').length);
  expect(dash).toBe(1);
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 't01-przeglad-telefon.png'), fullPage: true });

  // Wzory wiadomości on the phone: the table turns into cards, nothing cut
  // off, and a row that opens nothing does not look like it would.
  await tab(page, 'schemas').click();
  const table = page.locator('#tb-panel > [data-tb-view-slot="schemas"] [data-role="table"]');
  await expect(table.locator('tbody tr')).toHaveCount(2, { timeout: 15000 });
  const cursors = await table.locator('tbody tr').evaluateAll((rows) => [...new Set(rows.map((r) => getComputedStyle(r).cursor))]);
  expect(cursors).not.toContain('pointer');
  await assertNoOverflow(page);
  const clipped = await table.evaluate((host) => [...host.shadowRoot.querySelectorAll('td')].filter((td) => td.getBoundingClientRect().width > 0 && td.scrollWidth > td.clientWidth + 1).length);
  expect(clipped).toBe(0);
  await page.screenshot({ path: path.join(SHOTS, 't08-wzory-telefon.png'), fullPage: true });
  // Nieprzetworzone on the phone: every row keeps its actions reachable.
  await tab(page, 'dlq').click();
  const dlqTable = page.locator('#tb-dlq-table');
  await expect(dlqTable.locator('tbody tr').first()).toBeVisible({ timeout: 15000 });
  const buttons = dlqTable.locator('tf-button');
  const count = await buttons.count();
  expect(count).toBeGreaterThanOrEqual(3);
  for (let i = 0; i < count; i += 1) {
    const b = buttons.nth(i);
    await b.scrollIntoViewIfNeeded();
    await expect(b).toBeVisible();
    const box = await b.boundingBox();
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(PHONE.width);
  }
  await buttons.filter({ hasText: 'Szczegóły' }).first().click({ trial: true });
  await buttons.filter({ hasText: 'Szczegóły' }).first().click();
  await expect(page.locator('#tb-dlq-detail .tb-dlq-detail')).toBeVisible();
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 't06-nieprzetworzone-telefon.png'), fullPage: true });
  // The instance picker gets the whole row: the name never runs into the chevron.
  const pickerWidth = await page.locator('#tb-instance-select').evaluate((el) => el.getBoundingClientRect().width);
  expect(pickerWidth).toBeGreaterThan(280);

  // Reached through the address (a deep link, then a reload), not a click:
  // the active tab still ends up centred, after the counters widened the tabs.
  const instance = hashParams(page).instance;
  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instance}&tab=schemas`);
  await page.reload();
  await expect(tab(page, 'schemas')).toHaveAttribute('aria-selected', 'true', { timeout: 20000 });
  await expect(page.locator('#tb-tabs tf-tab#schemas')).toHaveAttribute('count', '2', { timeout: 15000 });
  await expect.poll(() => page.locator('#tb-tabs').evaluate((host) => {
    const strip = host.querySelector('[role="tablist"]').getBoundingClientRect();
    const active = host.querySelector('.tf-tab.active').getBoundingClientRect();
    return Math.round(Math.abs((active.left + active.width / 2) - (strip.left + strip.width / 2)));
  }), { timeout: 5000 }).toBeLessThan(20);
  expect(errors, errors.join('\n')).toEqual([]);
});

test('main tabs: each one shows its content, the address and the breadcrumb follow', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instance = await openInstance(page, 'Produkcja');
  const expectations = {
    topics: async () => {
      await expect(topicsSlot(page).locator('[data-role="table"] tbody tr')).toHaveCount(3, { timeout: 15000 });
      await expect(topicsSlot(page).locator('[data-role="footer"]')).toContainText('8 partycji');
    },
    groups: async () => {
      const rows = page.locator('#tb-groups-table tbody tr');
      await expect(rows).toHaveCount(4, { timeout: 15000 });
      const text = (await rows.allTextContents()).join(' | ');
      // "Czeka" per consumer — the same figures Przegląd counts (3 of 4 wait).
      expect(norm(text)).toContain('2 237');
      expect(norm(text)).toContain('400');
      expect(text).toContain('program sam daje znać, że skończył');
    },
    dlq: async () => expect(page.locator('#tb-dlq-source')).toBeVisible(),
    schemas: async () => {
      const slot = page.locator('#tb-panel > [data-tb-view-slot="schemas"]');
      await expect(slot.locator('[data-role="table"] tbody tr')).toHaveCount(2, { timeout: 15000 });
      await expect(slot.locator('[data-role="filter"] .tf-seg-opt')).toHaveText(['Wszystkie 2', 'W użyciu 1', 'Wycofane 1']);
    },
    replication: async () => {
      // The same per-node figures as Przegląd: 8 partitions of the 3 topics.
      const card = page.locator('#tb-repl-nodes .tb-node-card').first();
      await expect(card.locator('[id$="-leader"]')).toHaveText('8', { timeout: 15000 });
      await expect(card.locator('[id$="-follower"]')).toHaveText('0');
      await expect(card.locator('[id$="-isr"]')).toHaveText('8');
    },
    overview: async () => expect(overview(page).locator('.tb-alert').first()).toBeVisible(),
  };
  const labels = { topics: 'Topiki', groups: 'Odbiorcy', dlq: 'Nieprzetworzone', schemas: 'Wzory wiadomości', replication: 'Kopie i nody' };
  for (const [id, check] of Object.entries(expectations)) {
    await tab(page, id).click();
    await expect(tab(page, id)).toHaveAttribute('aria-selected', 'true');
    await check();
    const params = hashParams(page);
    expect(params.instance).toBe(instance);
    expect(params.tab).toBe(id === 'overview' ? undefined : id);
    if (id === 'dlq') {
      // Opens on the topic with the most unprocessed messages, named in the address.
      await expect(page.locator('#tb-dlq-source select')).toHaveValue('wyniki-badan');
      await expect.poll(() => hashParams(page).source).toBe('wyniki-badan');
      await expect(page.locator('#tb-dlq-body')).not.toContainText('nie ma nieprzetworzonych');
    }
    const crumbs = id === 'overview' ? ['TentaBus', 'Produkcja'] : ['TentaBus', 'Produkcja', labels[id]];
    await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(crumbs);
    await assertNoBannedWords(page);
    await assertNoInternalTopics(page);
    await assertSentenceCaseChips(page);
  }
  // The breadcrumb leads back to the overview.
  await tab(page, 'groups').click();
  await page.locator('#tb-crumbs a.tf-breadcrumb-item', { hasText: 'Produkcja' }).click();
  await expect(tab(page, 'overview')).toHaveAttribute('aria-selected', 'true');
  expect(errors, errors.join('\n')).toEqual([]);
});

test('alert and row buttons lead to the consumer, the unprocessed messages, the topic', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openInstance(page, 'Produkcja');
  const ov = overview(page);
  await expect(ov.locator('.tb-alert')).toHaveCount(3, { timeout: 20000 });

  await ov.locator('.tb-alert', { hasText: 'aplikacja-lekarza' }).locator('tf-button').click();
  await expect(tab(page, 'groups')).toHaveAttribute('aria-selected', 'true');
  await expect(page.locator('#tb-group-detail')).toContainText('aplikacja-lekarza');
  expect(hashParams(page)).toMatchObject({ tab: 'groups', group: 'aplikacja-lekarza', gtopic: 'wyniki-badan' });

  await tab(page, 'overview').click();
  await ov.locator('.tb-alert', { hasText: 'Przybywa' }).locator('tf-button').click();
  await expect(tab(page, 'dlq')).toHaveAttribute('aria-selected', 'true');
  await expect(page.locator('#tb-dlq-source select')).toHaveValue('wyniki-badan');
  await expect.poll(() => hashParams(page).source).toBe('wyniki-badan');
  await page.reload();
  await expect(page.locator('#tb-dlq-source select')).toHaveValue('wyniki-badan', { timeout: 20000 });
  await expect(page.locator('#tb-dlq-body')).not.toContainText('nie ma nieprzetworzonych');

  await tab(page, 'overview').click();
  await ov.locator('tf-stat-card[data-kpi="dlq"]').click();
  await expect(tab(page, 'dlq')).toHaveAttribute('aria-selected', 'true');
  await tab(page, 'overview').click();
  await ov.locator('tf-stat-card[data-kpi="groups"]').focus();
  await page.keyboard.press('Enter');
  await expect(tab(page, 'groups')).toHaveAttribute('aria-selected', 'true');
  await tab(page, 'overview').click();
  await ov.locator('[data-role="nodes"] .job-row').first().click();
  await expect(tab(page, 'replication')).toHaveAttribute('aria-selected', 'true');
  await tab(page, 'overview').click();
  await ov.locator('.topic-mini[data-topic="faktury"]').click();
  await expect(tab(page, 'topics')).toHaveAttribute('aria-selected', 'true');
  expect(hashParams(page)).toMatchObject({ tab: 'topics', topic: 'faktury' });
  await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(['TentaBus', 'Produkcja', 'Topiki', 'faktury']);
  expect(errors, errors.join('\n')).toEqual([]);
});

test('reload keeps the tab, the open topic and the open consumer', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instance = await openInstance(page, 'Produkcja');

  await tab(page, 'schemas').click();
  await page.reload();
  await expect(tab(page, 'schemas')).toHaveAttribute('aria-selected', 'true', { timeout: 20000 });
  await expect(page.locator('#tb-panel > [data-tb-view-slot="schemas"] [data-role="table"] tbody tr')).toHaveCount(2);

  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instance}&tab=dlq&source=wizyty`);
  await page.reload();
  await expect(page.locator('#tb-dlq-source select')).toHaveValue('wizyty', { timeout: 20000 });
  expect(hashParams(page).source).toBe('wizyty');

  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instance}&tab=topics&topic=wizyty`);
  await page.reload();
  await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(['TentaBus', 'Produkcja', 'Topiki', 'wizyty'], { timeout: 20000 });
  await expect(page.locator('#tb-detail-hero')).toContainText('wizyty', { timeout: 15000 });

  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instance}&tab=groups&group=system-rozliczen&gtopic=faktury`);
  await page.reload();
  await expect(page.locator('#tb-group-detail')).toContainText('system-rozliczen', { timeout: 20000 });
  await expect(tab(page, 'groups')).toHaveAttribute('aria-selected', 'true');
  expect(errors, errors.join('\n')).toEqual([]);
});

test('switching to the empty instance: T11 empty states, counters at zero, no alerts', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const production = await openInstance(page, 'Produkcja');
  await page.locator('#tb-instance-select select').selectOption({ label: 'Szkolenia' });
  await expect(page).not.toHaveURL(new RegExp(production));
  await expect(page.locator('#tb-head-sub')).toContainText('instancja Szkolenia', { timeout: 15000 });
  const ov = overview(page);
  await expect(ov.locator('tf-empty-state')).toHaveAttribute('title', 'Instancja Szkolenia jest pusta', { timeout: 20000 });
  const deltas = await ov.locator('tf-stat-card').evaluateAll((els) => els.map((e) => e.getAttribute('delta')));
  expect(deltas).toEqual(['nie ma jeszcze topików', 'utwórz pierwszy w zakładce Topiki', 'pojawią się, gdy programy zaczną czytać', 'nic nie czeka']);
  await expect(ov.locator('.tb-alert')).toHaveCount(0);
  const counts = await page.locator('#tb-tabs tf-tab').evaluateAll((tabs) => tabs.map((t) => t.getAttribute('count')));
  expect(counts).toEqual([null, '0', '0', '0', '0', '1']);
  await expect(page.locator('.tb-app-head [data-role="dlq"]')).toHaveAttribute('label', '0 nieprzetworzonych wiadomości');

  await ov.locator('tf-empty-state tf-button').click();
  await expect(tab(page, 'topics')).toHaveAttribute('aria-selected', 'true');
  await assertNoInternalTopics(page);
  await tab(page, 'dlq').click();
  await expect(page.locator('#tb-dlq-body tf-empty-state')).toHaveAttribute('title', 'Wszystkie wiadomości są przetworzone');
  await expect(page.locator('#tb-dlq-body tf-empty-state')).toHaveAttribute('badge', '');
  await expect(page.locator('#tb-dlq-toolbar')).toBeHidden();
  await expect(page.locator('#tb-dlq-retry-all')).toBeHidden();
  await assertNoInternalTopics(page);
  await tab(page, 'replication').click();
  await expect(page.locator('#tb-repl-matrix-body')).toContainText('Ta instancja nie ma jeszcze topików.');
  await expect(page.locator('#tb-repl-lag-state')).toContainText('Ta instancja nie ma jeszcze topików.');
  await expect(page.locator('#tb-repl-nodes .tb-node-sub').first()).not.toContainText('ms temu');
  await assertNoInternalTopics(page);
  await tab(page, 'schemas').click();
  await expect(page.locator('#tb-panel > [data-tb-view-slot="schemas"] tf-empty-state')).toHaveAttribute('title', 'Nie ma jeszcze wzorów wiadomości');

  await page.setViewportSize(PHONE);
  await tab(page, 'overview').click();
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 't11-szkolenia-przeglad-telefon.png'), fullPage: true });
  expect(errors, errors.join('\n')).toEqual([]);
});

// ----------------------------------------------------------------------------
// U1 — Topiki (T02), the topic creator (T04), delete and the message preview.
// ----------------------------------------------------------------------------

const topicsTable = (page) => topicsSlot(page).locator('[data-role="table"]');
const topicRow = (page, name) => topicsTable(page).locator('tbody tr', { hasText: name });
const creator = (page) => page.locator('tf-window.tb-creator');
const nextButton = (page) => creator(page).locator('[data-act="next"]');

async function openTopics(page, instanceName = 'Produkcja') {
  await openInstance(page, instanceName);
  await tab(page, 'topics').click();
  await expect(tab(page, 'topics')).toHaveAttribute('aria-selected', 'true');
}

test('T02 at 1440x900: rows with content and pattern, filters with counts, footer, row actions', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openTopics(page);
  const rows = topicsTable(page).locator('tbody tr');
  await expect(rows).toHaveCount(3, { timeout: 15000 });
  // What each topic carries and what checks it, from the server's own list.
  await expect(topicRow(page, 'wyniki-badan')).toContainText('HL7 v2 · bez wzoru');
  await expect(topicRow(page, 'wizyty')).toContainText('JSON · wzór wizyta');
  await expect(topicRow(page, 'faktury')).toContainText('XML · bez wzoru');
  // A consumer that falls behind or is paused makes its topic "opóźniony":
  // the waiting count turns into a chip, and the unprocessed messages are counted.
  await expect(topicRow(page, 'wyniki-badan').locator('.tf-chip', { hasText: 'czeka' })).toBeVisible();
  await expect(topicRow(page, 'faktury').locator('.tf-chip', { hasText: 'czeka' })).toBeVisible();
  await expect(topicRow(page, 'wizyty').locator('.tf-chip', { hasText: 'czeka' })).toHaveCount(0);
  expect(norm(await topicRow(page, 'wyniki-badan').innerText())).toContain('14');
  const filter = topicsSlot(page).locator('[data-role="filter"] .tf-seg-opt');
  await expect(filter).toHaveText(['Wszystkie 3', 'Opóźnione 2', 'Nieprzetworzone 1']);
  await expect(topicsSlot(page).locator('[data-role="count"]')).toHaveAttribute('label', '3');
  await expect(topicsSlot(page).locator('[data-role="footer"]')).toContainText('3 topiki');
  await expect(topicsSlot(page).locator('[data-role="footer"]')).toContainText('8 partycji');
  await expect(topicsSlot(page).locator('.section-card-head')).toContainText('Kliknij wiersz, aby otworzyć topik.');

  // Filters and search narrow the rows and the footer with them.
  await filter.filter({ hasText: 'Opóźnione' }).click();
  await expect(rows).toHaveCount(2);
  await expect(topicsSlot(page).locator('[data-role="footer"]')).toContainText('2 topiki');
  await filter.filter({ hasText: 'Nieprzetworzone' }).click();
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText('wyniki-badan');
  await filter.filter({ hasText: 'Wszystkie' }).click();
  await topicsSlot(page).locator('[data-role="search"] input').fill('fakt');
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText('faktury');
  await topicsSlot(page).locator('[data-role="search"] input').fill('nic-takiego');
  await expect(topicsSlot(page).locator('[data-role="no-match"]')).toBeVisible();
  await topicsSlot(page).locator('[data-role="search"] input').fill('');
  await expect(rows).toHaveCount(3);

  // Each row: the eye and the bin act on their own, the arrow and the row open it.
  const actions = await topicRow(page, 'wizyty').locator('tf-button').evaluateAll((els) => els.map((b) => b.dataset.act));
  expect(actions).toEqual(['preview', 'delete', 'open']);
  await assertNoOverflow(page);
  await assertNoBannedWords(page);
  await assertNoInternalTopics(page);
  await assertSentenceCaseChips(page);
  await page.screenshot({ path: path.join(SHOTS, 't02-topiki.png'), fullPage: true });

  await topicRow(page, 'faktury').click();
  await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(['TentaBus', 'Produkcja', 'Topiki', 'faktury']);
  expect(hashParams(page)).toMatchObject({ tab: 'topics', topic: 'faktury' });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('T04 creator: name checks, three steps, pattern for the chosen content, the new row', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openTopics(page);
  await expect(topicsTable(page).locator('tbody tr')).toHaveCount(3, { timeout: 15000 });
  await topicsSlot(page).locator('tf-button[data-go="create"]').click();
  await expect(creator(page).locator(".install-header")).toBeVisible();
  await expect(page.locator('.tf-window-backdrop')).toHaveCount(1);

  // Step 1: Dalej stays locked until the name can be used.
  await expect(nextButton(page)).toHaveAttribute('disabled', '');
  const name = creator(page).locator('#tb-cr-name');
  await name.locator('input').fill('wyniki-badan');
  await expect(name.locator('.tf-error-text')).toHaveText('Topik o tej nazwie już jest w tej instancji.');
  await expect(nextButton(page)).toHaveAttribute('disabled', '');
  await name.locator('input').fill('Wyniki Nowe');
  await expect(name.locator('.tf-error-text')).toContainText('małe litery');
  await name.locator('input').fill('__wewnetrzny');
  await expect(name.locator('.tf-error-text')).toContainText('„__”');
  await name.locator('input').fill('wyniki-z-pracowni');
  await expect(name.locator('.tf-error-text')).toBeHidden();
  await expect(creator(page).locator('[data-role="heading"]')).toHaveText('wyniki-z-pracowni');
  await expect(creator(page).locator('#tb-cr-kind tf-choice-card')).toHaveCount(3);
  await creator(page).locator('#tb-cr-kind tf-choice-card[value="application/json"]').click();
  await creator(page).locator('#tb-cr-partitions .tf-input-step--inc').click();
  await expect(creator(page).locator('#tb-cr-partitions input')).toHaveValue('4');
  await page.screenshot({ path: path.join(SHOTS, 't04-krok1.png') });
  await nextButton(page).click();

  // Step 2: the copies sentence is the server's own resolution (one node here).
  await expect(creator(page).locator('.install-step.done')).toHaveCount(1);
  await expect(creator(page).locator('.tb-copies-box')).toContainText('1 kopia, bo ta instancja ma jeden node');
  await expect(creator(page).locator('.tb-stat-rows')).toContainText('usuwaj stare wiadomości');
  await creator(page).locator('#tb-cr-retention select').selectOption('90');
  await creator(page).locator('#tb-cr-durability tf-choice-card[value="critical"]').click();
  await page.screenshot({ path: path.join(SHOTS, 't04-krok2.png') });
  await nextButton(page).click();

  // Step 3: only JSON patterns that are not withdrawn, checking on, the summary.
  await expect(creator(page).locator('#tb-cr-validate')).toHaveAttribute('checked', '');
  const options = await creator(page).locator('#tb-cr-schema select option').allTextContents();
  expect(options.map(norm)).toEqual(['wizyta · JSON Schema']);
  const summary = norm(await creator(page).locator('.tb-kv-grid').innerText());
  expect(summary).toContain('wyniki-z-pracowni');
  expect(summary).toContain('90 dni, usuwaj stare wiadomości');
  expect(summary).toContain('1, bo ta instancja ma jeden node');
  expect(summary).toContain('krytyczna');
  expect(summary).toContain('wizyta, najnowsza wersja');
  await page.screenshot({ path: path.join(SHOTS, 't04-krok3.png') });
  // Wstecz keeps what was chosen.
  await creator(page).locator('[data-act="back"]').click();
  await expect(creator(page).locator('#tb-cr-retention select')).toHaveValue('90');
  await nextButton(page).click();
  await expect(nextButton(page)).toContainText('Utwórz topik');
  await nextButton(page).click();

  // The result: window gone, the note above the list, the new row from the server.
  await expect(creator(page)).toHaveCount(0);
  await expect(page.locator('.tf-window-backdrop')).toHaveCount(0);
  const notice = topicsSlot(page).locator('[data-role="notice"] tf-alert');
  await expect(notice).toHaveAttribute('title', 'Utworzono topik wyniki-z-pracowni.');
  await expect(notice).toHaveAttribute('message', /sprawdza je wzór wizyta/);
  await expect(topicsTable(page).locator('tbody tr')).toHaveCount(4, { timeout: 15000 });
  const row = topicRow(page, 'wyniki-z-pracowni');
  await expect(row).toContainText('JSON · wzór wizyta');
  await expect(row).toContainText('90 dni');
  await expect(page.locator('#tb-tabs tf-tab#topics')).toHaveAttribute('count', '4', { timeout: 15000 });
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 't04-utworzono.png'), fullPage: true });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('message preview: the busiest partition at its newest page, the newest message open, audit note', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openTopics(page);
  await topicRow(page, 'wyniki-badan').locator('tf-button[data-act="preview"]').click();
  const win = page.locator('tf-window.tb-preview-window');
  await expect(win.locator(".tb-audit-banner")).toBeVisible();
  await expect(win.locator('.tb-audit-banner')).toContainText('Ten podgląd zapisuje się w dzienniku audytu.');
  await expect(win.locator('[data-role="table"] tbody tr').first()).toBeVisible({ timeout: 15000 });
  const count = await win.locator('[data-role="table"] tbody tr').count();
  expect(count).toBeGreaterThan(0);
  expect(count).toBeLessThanOrEqual(50);
  await expect(win.locator('[data-role="range"]')).toHaveText(/^od \d[\d\s\u00a0\u202f]* do \d[\d\s\u00a0\u202f]*$/);
  await expect(win.locator('.tb-payload')).toContainText('MSH|');
  await expect(win.locator('.tb-preview-record-head')).toContainText(/wiadomość \d[\d\s\u00a0\u202f]* · partycja \d · zapisana/i);
  await expect(win.locator('[data-role="note"]')).toContainText('Od najstarszej do najnowszej.');
  // Another partition from its first message, then a row opens its content.
  await win.locator('[data-role="partition"] select').selectOption('0');
  await win.locator('[data-role="from"] input').fill('0');
  await win.locator('[data-role="from"] input').press('Enter');
  await expect(win.locator('[data-role="table"] tbody tr').first()).toContainText('Partycja 0', { timeout: 15000 });
  await win.locator('[data-role="table"] tbody tr').first().click();
  await expect(win.locator('.tb-preview-record-head')).toContainText('Wiadomość 0 · partycja 0');
  await expect(win.locator('[data-role="more"]')).toBeVisible();
  const before = await win.locator('[data-role="table"] tbody tr').count();
  await win.locator('[data-role="more"]').click();
  await expect.poll(() => win.locator('[data-role="table"] tbody tr').count()).toBeGreaterThan(before);
  await page.screenshot({ path: path.join(SHOTS, 't02-podglad.png') });
  await win.locator('tf-button', { hasText: 'Zamknij' }).click();
  await expect(win).toHaveCount(0);
  expect(errors, errors.join('\n')).toEqual([]);
});

// One binary-protocol call from the page, as the screen itself makes it.
function busCall(page, kind, payload) {
  return page.evaluate(async ([k, p]) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return k.endsWith('ListRequest') ? ApiBinary.one(k, p) : ApiBinary.action(k, p);
  }, [kind, payload]);
}

test('delete with the retyped name: what goes, what stays, the list without it', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openTopics(page);
  await expect(topicRow(page, 'wyniki-z-pracowni')).toHaveCount(1, { timeout: 15000 });
  const instanceId = hashParams(page).instance;
  const topic = 'wyniki-z-pracowni';
  await busCall(page, 'busAclSetRequest', { instanceId, topic, subjectType: 'user', subjectId: 'e2e-dawny-odbiorca', accessLevel: 'deny', action: 'read' });
  await topicRow(page, 'wyniki-z-pracowni').locator('tf-button[data-act="delete"]').click();
  const win = page.locator('tf-window.tb-delete-window');
  await expect(win.locator(".tb-danger-box")).toBeVisible();
  await expect(win.locator('.tb-danger-box')).toContainText('Tej operacji nie da się cofnąć.');
  await expect(win.locator('.tb-impact-list')).toContainText('4 puste partycje');
  await expect(win.locator('.tb-impact-list')).toContainText('1 wpis dostępu');
  await expect(win.locator('.tb-kept-box')).toContainText('wzór wiadomości wizyta');
  const confirm = win.locator('tf-button[data-action="confirm"]');
  await expect(confirm).toHaveAttribute('disabled', '');
  await win.locator('#retype-input input').fill('wyniki-z');
  await expect(confirm).toHaveAttribute('disabled', '');
  await win.locator('#retype-input input').fill('wyniki-z-pracowni');
  await expect(confirm).not.toHaveAttribute('disabled', '');
  await page.screenshot({ path: path.join(SHOTS, 't02-usun.png') });
  await confirm.click();
  await expect(win).toHaveCount(0);
  const notice = topicsSlot(page).locator('[data-role="notice"] tf-alert');
  await expect(notice).toHaveAttribute('title', 'Usunięto topik wyniki-z-pracowni.');
  await expect(topicsTable(page).locator('tbody tr')).toHaveCount(3, { timeout: 15000 });
  await expect(topicRow(page, 'wyniki-z-pracowni')).toHaveCount(0);
  await expect(page.locator('#tb-tabs tf-tab#topics')).toHaveAttribute('count', '3', { timeout: 15000 });
  // A topic created again under the same name starts with no access entries
  // and no data-hiding rules of the deleted one.
  await busCall(page, 'busTopicCreateRequest', { instanceId, name: topic, options: { partitions: 1 } });
  const acl = await busCall(page, 'busAclListRequest', { instanceId, topic });
  expect(acl?.entries || []).toEqual([]);
  const policies = await busCall(page, 'busFieldPolicyListRequest', { instanceId, topic });
  expect(policies?.policies || []).toEqual([]);
  await busCall(page, 'busTopicDeleteRequest', { instanceId, name: topic });
  // Leaving the tab drops the note.
  await tab(page, 'overview').click();
  await tab(page, 'topics').click();
  await expect(topicsSlot(page).locator('[data-role="notice"] tf-alert')).toHaveCount(0);
  expect(errors, errors.join('\n')).toEqual([]);
});

test('T02/T04 at 390x844: cards, no horizontal scroll, the creator and the preview fit the phone', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(PHONE);
  await login(page);
  await openTopics(page);
  await expect(topicsTable(page).locator('tbody tr')).toHaveCount(3, { timeout: 15000 });
  await assertNoOverflow(page);
  const clipped = await topicsTable(page).evaluate((host) => [...host.shadowRoot.querySelectorAll('td')].filter((td) => td.getBoundingClientRect().width > 0 && td.scrollWidth > td.clientWidth + 1).length);
  expect(clipped).toBe(0);
  const buttons = topicRow(page, 'faktury').locator('tf-button');
  for (let i = 0; i < await buttons.count(); i += 1) {
    const box = await buttons.nth(i).boundingBox();
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(PHONE.width);
  }
  await page.screenshot({ path: path.join(SHOTS, 't02-topiki-telefon.png'), fullPage: true });

  const fits = async (sel) => {
    const box = await page.locator(sel).evaluate((w) => {
      const r = (w.shadowRoot?.querySelector('.tf-window') || w).getBoundingClientRect();
      return { left: r.left, right: r.right };
    });
    expect(box.left).toBeGreaterThanOrEqual(0);
    expect(box.right).toBeLessThanOrEqual(PHONE.width);
    const overflow = await page.locator(sel).evaluate((w) => {
      const out = [];
      for (const el of w.querySelectorAll('tf-input, tf-select, tf-choice-card, .tb-copies-box, .tb-kv-grid, .install-step, tf-button, tf-table')) {
        const b = el.getBoundingClientRect();
        if (b.width && (b.left < -1 || b.right > window.innerWidth + 1)) out.push(el.tagName + '.' + el.className);
      }
      return out;
    });
    expect(overflow, overflow.join('\n')).toEqual([]);
  };
  await topicsSlot(page).locator('tf-button[data-go="create"]').click();
  await expect(creator(page).locator(".install-header")).toBeVisible();
  await fits('tf-window.tb-creator');
  await creator(page).locator('#tb-cr-name input').fill('telefon-test');
  await nextButton(page).click();
  await fits('tf-window.tb-creator');
  await page.screenshot({ path: path.join(SHOTS, 't04-krok2-telefon.png') });
  await creator(page).locator('[data-act="cancel"]').click();
  await expect(creator(page)).toHaveCount(0);
  await expect(topicsTable(page).locator('tbody tr')).toHaveCount(3);

  await topicRow(page, 'wizyty').locator('tf-button[data-act="preview"]').click();
  await expect(page.locator('tf-window.tb-preview-window .tb-payload')).toBeVisible({ timeout: 15000 });
  await fits('tf-window.tb-preview-window');
  await page.screenshot({ path: path.join(SHOTS, 't02-podglad-telefon.png') });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('T11 Szkolenia: the empty list leads to the creator with one copy and no patterns', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openTopics(page, 'Szkolenia');
  const empty = topicsSlot(page).locator('tf-empty-state');
  await expect(empty).toHaveAttribute('title', 'Nie ma jeszcze żadnego topiku', { timeout: 15000 });
  await empty.locator('tf-button[data-go="create"]').click();
  await creator(page).locator('#tb-cr-name input').fill('wyniki-z-pracowni');
  await nextButton(page).click();
  await expect(creator(page).locator('.tb-copies-box')).toContainText('1 kopia, bo ta instancja ma jeden node. Gdy dołączysz kolejne nody');
  await nextButton(page).click();
  await expect(creator(page).locator('.tb-explain-box')).toContainText('W instancji Szkolenia nie ma jeszcze wzorów wiadomości');
  await expect(creator(page).locator('.tb-kv-grid')).toContainText('bez wzoru');
  await page.screenshot({ path: path.join(SHOTS, 't11-szkolenia-nowy-krok3.png') });
  // Closing leaves the instance as empty as it was.
  await page.keyboard.press('Escape');
  await expect(creator(page)).toHaveCount(0);
  await expect(empty).toBeVisible();
  expect(errors, errors.join('\n')).toEqual([]);
});

test('T12: when the node stops, the list keeps its last data under the connection notice', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openTopics(page);
  await expect(topicsTable(page).locator('tbody tr')).toHaveCount(3, { timeout: 15000 });
  await stopAndWait(server);
  server = null;
  // The whole dashboard lost its node: the platform's connection notice takes
  // over, and the screen underneath keeps what it last showed instead of
  // blanking it. (A failed list load with a live node — the tab's own error
  // card — is covered by topics.test.js.)
  await expect(page.locator('.conn-overlay.visible')).toBeVisible({ timeout: 30000 });
  await expect(topicsTable(page).locator('tbody tr')).toHaveCount(3);
  await expect(topicRow(page, 'wyniki-badan')).toContainText('HL7 v2');
  await page.screenshot({ path: path.join(SHOTS, 't12-topiki-bez-polaczenia.png'), fullPage: true });
  // Transport noise of the stopped node is the point of this test, not a defect.
  expect(errors.filter((e) => !/WebSocket|WebTransport|net::|ERR_CONNECTION|Failed to fetch|socket|timed out|protocol error/i.test(e)), errors.join('\n')).toEqual([]);
});
