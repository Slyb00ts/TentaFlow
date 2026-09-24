// =============================================================================
// File: tests/e2e/tentabus-ui.spec.js
// Description: TentaBus screen shell and Przegląd (PLAN-UI-20260923 U0) on a
//              real node seeded with the "Przychodnia Zdrowie" world
//              (`seed_clinic_data` in tentaflow-core/tests/bus_demo_seed.rs):
//              boot once to migrate, seed offline, boot again. Drives T01 at
//              1440x900 and 390x844, the six main tabs, the alerts' buttons,
//              reload of a tab and of a topic, and switching to the empty
//              instance (T11). Stateful: run the whole project, never `-g`.
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
      await expect(page.locator('#tb-topics-table tbody tr')).toHaveCount(3, { timeout: 15000 });
      await expect(page.locator('#tb-kpi-topics')).toHaveAttribute('value', '3');
      await expect(page.locator('#tb-kpi-partitions')).toHaveAttribute('value', '8');
    },
    groups: async () => expect(page.locator('#tb-groups-table tbody tr')).toHaveCount(4, { timeout: 15000 }),
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
  await assertNoInternalTopics(page);
  await tab(page, 'schemas').click();
  await expect(page.locator('#tb-panel > [data-tb-view-slot="schemas"] tf-empty-state')).toHaveAttribute('title', 'Nie ma jeszcze wzorów wiadomości');

  await page.setViewportSize(PHONE);
  await tab(page, 'overview').click();
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 't11-szkolenia-przeglad-telefon.png'), fullPage: true });
  expect(errors, errors.join('\n')).toEqual([]);
});
