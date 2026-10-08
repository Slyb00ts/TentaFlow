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
//              layout, the empty instance's creator; then U2: a topic's page
//              (Stan, Ustawienia with its five windows and delete, Partycje
//              i kopie), Kopie i nody, the phone layout and a reader without
//              administration or read access; then U3: Odbiorcy (the list,
//              its filters, pause and resume), a consumer's page (Stan,
//              Miejsce czytania, Ustawienia), moving the reading place each
//              way with the counted consequence checked against the server,
//              the phone layout and a reader without rights; then U4:
//              Nieprzetworzone wiadomości (the tiles and the merged list of the
//              instance, a topic's section, Pokaż with the data-hiding rules
//              applied, Ponów / Odrzuć / Ponów wszystkie with every counter
//              following, the phone layout and a reader without rights); then
//              U5: Wzory wiadomości (the list, a pattern's page with the text
//              of a version, "Pobierz" and "Kopiuj", adding a pattern, a new
//              version refused in plain words and added after the
//              compatibility changed, withdrawing a version and the pattern,
//              deleting from the page and from the list, a used pattern the
//              server will not delete, the phone layout and a reader); then
//              U6: a topic's Dostęp (a group given reading, a person a write
//              ban, a change, the ban removed with its warning, an addon;
//              a key issued with reading and shown once, checked over the
//              records REST — it reads and cannot write — its rights changed
//              and the key revoked; the phone layout; no section without
//              administration); then U7: a topic's Ukrywanie danych (the empty
//              state, a group rule added and changed through its window, the
//              preview as the group with the audit entry and the fields the rules
//              hid, the preview narrowed by the administrator's own rule explained,
//              a writing rule, a rule for everyone, every rule deleted; an HL7
//              topic with the dictionary and a refused address; a binary topic
//              that cannot get a rule; the phone layout; no section without
//              administration); and, last because it stops the node, the
//              list kept under the connection notice (T12). Stateful: run the whole project, never `-g`.
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
const consumersSlot = (page) => page.locator('#tb-panel > [data-tb-view-slot="groups"]');
const consumerSlot = (page) => page.locator('#tb-panel > [data-tb-view-slot="consumer"]');
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
  await expect(head.locator('[data-role="dlq"]')).toHaveAttribute('label', '17 nieprzetworzonych wiadomości');
  await expect(head.locator('[data-role="schemas"]')).toHaveAttribute('label', '2 wzory wiadomości');
  await expect(page.locator('#tb-instance-select select')).toHaveValue(/tentabus-/);

  // Breadcrumb and the six tabs with their counters.
  await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(['TentaBus', 'Produkcja']);
  const counts = await page.locator('#tb-tabs tf-tab').evaluateAll((tabs) => tabs.map((t) => [t.id, t.getAttribute('count')]));
  expect(counts).toEqual([['overview', null], ['topics', '3'], ['groups', '4'], ['dlq', '17'], ['schemas', '2'], ['replication', '1']]);
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
  await expect(tile('dlq')).toHaveAttribute('value', '17');

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
    'Przybywa nieprzetworzonych wiadomości',
    'Odbiorca system-rozliczen jest wstrzymany',
  ]);
  // Nobody consumes or produces during the run: the backlog waits, it does not grow.
  await expect(ov.locator('.tb-alert').first()).toContainText(/czeka od 2\d min/);
  await expect(ov.locator('.tb-alert').nth(1)).toContainText('14 w ostatniej godzinie, razem 14');
  await expect(ov.locator('.tb-alert').nth(2)).toContainText('3 w ostatniej godzinie, razem 3');
  await expect(ov.locator('[data-role="alerts-count"] tf-chip')).toHaveAttribute('label', '4');

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
  await expect(ov.locator('.tb-alert')).toHaveCount(4, { timeout: 20000 });
  const cols = await ov.locator('.tb-kpi').evaluate((el) => getComputedStyle(el).gridTemplateColumns.split(' ').length);
  expect(cols).toBe(2);
  const dash = await ov.locator('.tb-dash').first().evaluate((el) => getComputedStyle(el).gridTemplateColumns.split(' ').length);
  expect(dash).toBe(1);
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 't01-przeglad-telefon.png'), fullPage: true });

  // Wzory wiadomości on the phone: the table turns into cards, nothing cut off.
  await tab(page, 'schemas').click();
  const table = page.locator('#tb-panel > [data-tb-view-slot="schemas"] [data-role="table"]');
  await expect(table.locator('tbody tr')).toHaveCount(2, { timeout: 15000 });
  await assertNoOverflow(page);
  const clipped = await table.evaluate((host) => [...host.shadowRoot.querySelectorAll('td')].filter((td) => td.getBoundingClientRect().width > 0 && td.scrollWidth > td.clientWidth + 1).length);
  expect(clipped).toBe(0);
  await page.screenshot({ path: path.join(SHOTS, 't08-wzory-telefon.png'), fullPage: true });
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
      const rows = consumersSlot(page).locator('[data-role="table"] tbody tr');
      await expect(rows).toHaveCount(4, { timeout: 15000 });
      const text = (await rows.allTextContents()).join(' | ');
      // "Czeka" per consumer — the same figures Przegląd counts (3 of 4 wait).
      expect(norm(text)).toContain('2 237');
      expect(norm(text)).toContain('400');
      expect(text).toContain('program sam daje znać, że skończył');
    },
    dlq: async () => {
      const slot = page.locator('#tb-panel > [data-tb-view-slot="dlq"]');
      await expect(slot.locator('.tb-unp-tile')).toHaveCount(2, { timeout: 15000 });
      await expect(slot.locator('[data-role="table"] tbody tr')).toHaveCount(10, { timeout: 15000 });
    },
    schemas: async () => {
      const slot = page.locator('#tb-panel > [data-tb-view-slot="schemas"]');
      await expect(slot.locator('[data-role="table"] tbody tr')).toHaveCount(2, { timeout: 15000 });
      await expect(slot.locator('[data-role="filter"] .tf-seg-opt')).toHaveText(['Wszystkie 2', 'W użyciu 1', 'Wycofane 1']);
    },
    replication: async () => {
      // The same per-node figures as Przegląd: 8 partitions of the 3 topics.
      const card = page.locator('#tb-panel > [data-tb-view-slot="replication"] .tb-node-card').first();
      await expect(card.locator('[data-role="leads"]')).toHaveText('8', { timeout: 15000 });
      await expect(card.locator('[data-role="holds"]')).toHaveText('0');
      await expect(card.locator('[data-role="sync"]')).toHaveText('8 z 8');
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
  await expect(ov.locator('.tb-alert')).toHaveCount(4, { timeout: 20000 });

  await ov.locator('.tb-alert', { hasText: 'aplikacja-lekarza' }).locator('tf-button').click();
  await expect(tab(page, 'groups')).toHaveAttribute('aria-selected', 'true');
  await expect(consumerSlot(page).locator('.tb-title')).toHaveText('aplikacja-lekarza', { timeout: 15000 });
  expect(hashParams(page)).toMatchObject({ tab: 'groups', group: 'aplikacja-lekarza', gtopic: 'wyniki-badan' });
  await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(['TentaBus', 'Produkcja', 'Odbiorcy', 'aplikacja-lekarza']);

  await tab(page, 'overview').click();
  // An arriving topic's unprocessed messages are dealt with in its own section.
  await ov.locator('.tb-alert', { hasText: 'Przybywa' }).first().locator('tf-button').click();
  await expect(tab(page, 'topics')).toHaveAttribute('aria-selected', 'true');
  await expect.poll(() => hashParams(page)).toMatchObject({ tab: 'topics', topic: 'wyniki-badan', section: 'dlq' });
  await page.reload();
  await expect(detailSlot(page).locator('[data-section="dlq"] [data-role="table"] tbody tr').first()).toBeVisible({ timeout: 20000 });

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

  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instance}&tab=topics&topic=wizyty&section=dlq`);
  await page.reload();
  await expect(detailSlot(page).locator('[data-role="menu"]')).toHaveAttribute('value', 'dlq', { timeout: 20000 });
  await expect(detailSlot(page).locator('[data-section="dlq"] [data-role="table"] tbody tr')).toHaveCount(3, { timeout: 15000 });

  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instance}&tab=topics&topic=wizyty`);
  await page.reload();
  await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(['TentaBus', 'Produkcja', 'Topiki', 'wizyty'], { timeout: 20000 });
  await expect(page.locator('#tb-panel > [data-tb-view-slot="detail"] .tb-title')).toHaveText('wizyty', { timeout: 15000 });

  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instance}&tab=groups&group=system-rozliczen&gtopic=faktury`);
  await page.reload();
  await expect(consumerSlot(page).locator('.tb-title')).toHaveText('system-rozliczen', { timeout: 20000 });
  await expect(tab(page, 'groups')).toHaveAttribute('aria-selected', 'true');
  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instance}&tab=groups&group=system-rozliczen&gtopic=faktury&section=settings`);
  await page.reload();
  await expect(consumerSlot(page).locator('[data-role="menu"]')).toHaveAttribute('value', 'settings', { timeout: 20000 });
  await expect(consumerSlot(page).locator('[data-section="settings"]')).toBeVisible();
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
  const unp = page.locator('#tb-panel > [data-tb-view-slot="dlq"]');
  await expect(unp.locator('tf-empty-state')).toHaveAttribute('title', 'Wszystkie wiadomości są przetworzone', { timeout: 15000 });
  await expect(unp.locator('tf-empty-state')).toHaveAttribute('message', /Teraz nie ma żadnej\.$/);
  await expect(unp.locator('tf-empty-state')).toHaveAttribute('badge', '');
  await expect(unp.locator('.tb-unp-tile')).toHaveCount(0);
  await expect(unp.locator('tf-button')).toHaveCount(0);
  await assertNoInternalTopics(page);
  await tab(page, 'groups').click();
  await expect(consumersSlot(page).locator('tf-empty-state')).toHaveAttribute('title', 'Nikt jeszcze nie czyta z tej instancji', { timeout: 15000 });
  await tab(page, 'replication').click();
  const repl = page.locator('#tb-panel > [data-tb-view-slot="replication"]');
  await expect(repl.locator('[data-role="topics-none"]')).toHaveText('Ta instancja nie ma jeszcze topików.', { timeout: 15000 });
  await expect(repl.locator('[data-role="attn-none"]')).toBeVisible();
  await expect(repl.locator('.tb-node-card [data-role="signal"]').first()).toHaveText('teraz (ten node)');
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
  await expect(filter).toHaveText(['Wszystkie 3', 'Opóźnione 2', 'Nieprzetworzone 2']);
  await expect(topicsSlot(page).locator('[data-role="count"]')).toHaveAttribute('label', '3');
  await expect(topicsSlot(page).locator('[data-role="footer"]')).toContainText('3 topiki');
  await expect(topicsSlot(page).locator('[data-role="footer"]')).toContainText('8 partycji');
  await expect(topicsSlot(page).locator('.section-card-head')).toContainText('Kliknij wiersz, aby otworzyć topik.');

  // Filters and search narrow the rows and the footer with them.
  await filter.filter({ hasText: 'Opóźnione' }).click();
  await expect(rows).toHaveCount(2);
  await expect(topicsSlot(page).locator('[data-role="footer"]')).toContainText('2 topiki');
  await filter.filter({ hasText: 'Nieprzetworzone' }).click();
  await expect(rows).toHaveCount(2);
  expect(norm((await rows.allTextContents()).join(' | '))).toMatch(/wyniki-badan.*\| .*wizyty|wizyty.*\| .*wyniki-badan/);
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
  // Opis: optional; over 500 characters locks Dalej with the reason.
  const description = creator(page).locator('#tb-cr-description');
  await description.locator('textarea').fill('x'.repeat(501));
  await expect(description.locator('.tf-error-text')).toContainText('najwyżej 500 znaków');
  await expect(nextButton(page)).toHaveAttribute('disabled', '');
  await description.locator('textarea').fill('Wyniki z pracowni dla aplikacji lekarza');
  await expect(description.locator('.tf-error-text')).toBeHidden();
  await expect(nextButton(page)).not.toHaveAttribute('disabled', '');
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
  expect(summary).toContain('Opis Wyniki z pracowni dla aplikacji lekarza');
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
  const created = await busCall(page, 'busTopicDetailRequest', { instanceId: hashParams(page).instance, name: 'wyniki-z-pracowni' });
  expect(created.topic.description).toBe('Wyniki z pracowni dla aplikacji lekarza');
  expect(created.topic.createdByLabel).toBeTruthy();
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
  // Row dividers on a phone: a flush table keeps its lines between rows,
  // while the card layout of an ordinary table draws none between fields.
  const topBorder = (loc) => loc.evaluate((el) => getComputedStyle(el).borderTopWidth);
  const flushRows = page.locator('tf-window.tb-preview-window tf-table[variant="flush"] tbody tr');
  await expect(flushRows.nth(1)).toBeVisible({ timeout: 15000 });
  expect(await topBorder(flushRows.nth(1).locator('td').first())).toBe('1px');
  expect(await topBorder(topicsTable(page).locator('tbody tr').nth(1).locator('td').nth(1))).toBe('0px');
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

// ----------------------------------------------------------------------------
// U2 — a topic's page (Stan / Ustawienia / Partycje i kopie), its four
// "Zmień" windows and delete, and the Kopie i nody tab (T10).
// ----------------------------------------------------------------------------

const detailSlot = (page) => page.locator('#tb-panel > [data-tb-view-slot="detail"]');
const section = (page, id) => detailSlot(page).locator(`[data-section="${id}"]`);
const changeWindow = (page) => page.locator('tf-window.tb-change-window');
const replicationSlot = (page) => page.locator('#tb-panel > [data-tb-view-slot="replication"]');

async function openTopicPage(page, topic, sectionId = null, instanceName = 'Produkcja') {
  const instance = await openInstance(page, instanceName);
  const params = new URLSearchParams({ instance, tab: 'topics', topic });
  if (sectionId) params.set('section', sectionId);
  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?${params.toString()}`);
  await expect(detailSlot(page).locator('.tb-title')).toHaveText(topic, { timeout: 20000 });
  return instance;
}

// The window fits the viewport and nothing inside it sticks out.
async function windowFits(page, sel, width) {
  const problems = await page.locator(sel).evaluate((w, vw) => {
    const out = [];
    const r = (w.shadowRoot?.querySelector('.tf-window') || w).getBoundingClientRect();
    if (r.left < -1 || r.right > vw + 1) out.push(`window ${Math.round(r.left)}..${Math.round(r.right)}`);
    for (const el of w.querySelectorAll('tf-input, tf-select, tf-segmented, tf-choice-card, tf-button, .tb-will-happen')) {
      const b = el.getBoundingClientRect();
      if (b.width && (b.left < -1 || b.right > vw + 1)) out.push(`${el.tagName}.${el.className}`);
    }
    return out;
  }, width);
  expect(problems, problems.join('\n')).toEqual([]);
}

test('U2 topic page at 1440: the menu, Stan with this topic\'s figures, alerts and consumers', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openTopics(page);
  await topicRow(page, 'wyniki-badan').click();
  const d = detailSlot(page);
  await expect(d.locator('.tb-title')).toHaveText('wyniki-badan', { timeout: 20000 });
  await expect(d.locator('[data-role="desc"]')).toHaveText('HL7 v2 · bez wzoru');
  await expect(tab(page, 'topics')).toHaveAttribute('aria-selected', 'true');
  await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(['TentaBus', 'Produkcja', 'Topiki', 'wyniki-badan']);
  const menu = d.locator('[data-role="menu"]');
  await expect(menu).toHaveAttribute('orientation', 'vertical');
  await expect(menu.locator('tf-tab')).toHaveText([/Stan/, /Ustawienia/, /Dostęp/, /Ukrywanie danych/, /Nieprzetworzone\s*14/, /Partycje i kopie\s*3/]);
  await expect(d.locator('[data-role="pick"]')).toBeHidden();

  const s = section(page, 'state');
  await expect(s.locator('tf-stat-card')).toHaveCount(4, { timeout: 15000 });
  await expect(s.locator('tf-stat-card[data-kpi="dlq"]')).toHaveAttribute('value', '14');
  await expect(s.locator('tf-stat-card[data-kpi="dlq"]')).toHaveAttribute('delta', '14 w ostatniej godzinie');
  await expect(s.locator('tf-stat-card[data-kpi="waiting"]')).toHaveAttribute('delta', 'aplikacja-lekarza nie nadąża');
  await expect(s.locator('tf-stat-card[data-kpi="disk"]')).toHaveAttribute('delta', /^najstarsza wiadomość z \d\d\.\d\d\.\d{4}$/);
  await expect(s.locator('.tb-alert')).toHaveCount(2);
  await expect(s.locator('.tb-consumer-row')).toHaveCount(2);
  await expect(s.locator('.tb-consumer-row').first()).toContainText('aplikacja-lekarza');
  await assertNoBannedWords(page);
  await assertNoOverflow(page);
  await assertSentenceCaseChips(page);
  await page.screenshot({ path: path.join(SHOTS, 'tp-stan.png'), fullPage: true });

  // Each section alone, named in the address; a reload comes back to it.
  await menu.locator('tf-tab#settings > button').click();
  await expect(section(page, 'settings')).toBeVisible();
  await expect(s).toBeHidden();
  await expect.poll(() => hashParams(page).section).toBe('settings');
  await page.reload();
  await expect(section(page, 'settings').locator('.section-card').first()).toBeVisible({ timeout: 20000 });
  await expect(menu).toHaveAttribute('value', 'settings');

  // Alert buttons lead to where the thing is dealt with.
  await menu.locator('tf-tab#state > button').click();
  await expect.poll(() => hashParams(page).section).toBe(undefined);
  await s.locator('.tb-alert', { hasText: '14 nieprzetworzonych' }).locator('tf-button').click();
  await expect(tab(page, 'topics')).toHaveAttribute('aria-selected', 'true');
  await expect(menu).toHaveAttribute('value', 'dlq');
  await expect.poll(() => hashParams(page).section).toBe('dlq');
  await expect(section(page, 'dlq').locator('[data-role="table"] tbody tr').first()).toBeVisible({ timeout: 15000 });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U2 Ustawienia: values to read with locks; Przechowywanie saved through its window, Anuluj changes nothing', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openTopicPage(page, 'wyniki-badan', 'settings');
  const s = section(page, 'settings');
  await expect(s.locator('.section-card')).toHaveCount(5, { timeout: 15000 });
  await expect(s.locator('[data-go="change"]')).toHaveCount(5);
  await expect(s.locator('.tb-danger-zone')).toContainText('Usuń topik wyniki-badan');
  // Author, cleanup, copies, durability, content kind.
  await expect(s.locator('.tb-vr-lock')).toHaveCount(5);
  await expect(s.locator('.section-card[data-card="write"]')).toContainText('Ustalone przy tworzeniu topiku');
  const retentionValue = s.locator('.section-card[data-card="retention"] .tb-vrow').first().locator('.tb-vr-value');
  await expect(retentionValue).toHaveText('7 dni');
  await page.screenshot({ path: path.join(SHOTS, 'tp-ustawienia.png'), fullPage: true });

  // Anuluj leaves everything as it was.
  await s.locator('[data-go="change"][data-card="retention"]').click();
  const win = changeWindow(page);
  await expect(win.locator('[data-act="save"]')).toHaveAttribute('disabled', '');
  await expect(win.locator('.tb-vr-lock')).toContainText('Innego sposobu sprzątania na razie nie ma.');
  await win.locator('#tb-set-retention select').selectOption({ label: '3 dni' });
  await expect(win.locator('[data-role="impact"]')).toContainText('Co się stanie po zapisaniu: Wiadomości');
  await win.locator('[data-act="cancel"]').click();
  await expect(win).toHaveCount(0);
  await expect(retentionValue).toHaveText('7 dni');

  // Zapisz: the sentence says what goes, the page shows "Zapisano" and the new value.
  await s.locator('[data-go="change"][data-card="retention"]').click();
  await win.locator('#tb-set-retention select').selectOption({ label: '14 dni' });
  await expect(win.locator('[data-role="impact"]')).toContainText('Wiadomości będą trzymane 14 dni.');
  await page.screenshot({ path: path.join(SHOTS, 'tp-ustawienia-zmien-przechowywanie.png') });
  await win.locator('[data-act="save"]').click();
  await expect(win).toHaveCount(0);
  await expect(s.locator('tf-alert[data-role="saved"]')).toHaveAttribute('title', 'Zapisano przechowywanie.');
  await expect(retentionValue).toHaveText('14 dni', { timeout: 15000 });
  const instanceId = hashParams(page).instance;
  const detail = await busCall(page, 'busTopicDetailRequest', { instanceId, name: 'wyniki-badan' });
  expect(detail.topic.retentionMs).toBe(14 * 86_400_000);
  // Leaving the section drops the note.
  await detailSlot(page).locator('[data-role="menu"] tf-tab#state > button').click();
  await detailSlot(page).locator('[data-role="menu"] tf-tab#settings > button').click();
  await expect(s.locator('tf-alert[data-role="saved"]')).toHaveCount(0);
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U2 Opis: written through its window, shown under the name, and cleared again', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openTopicPage(page, 'wyniki-badan', 'settings');
  const s = section(page, 'settings');
  const about = s.locator('.section-card[data-card="about"]');
  await expect(about).toContainText('Brak opisu', { timeout: 15000 });
  await expect(about).toContainText('Utworzył');
  const desc = detailSlot(page).locator('[data-role="desc"]');
  const text = 'Wyniki badań z pracowni dla aplikacji lekarza';

  await s.locator('[data-go="change"][data-card="about"]').click();
  const win = changeWindow(page);
  await expect(win.locator('[data-act="save"]')).toHaveAttribute('disabled', '');
  await win.locator('#tb-set-description textarea').fill(text);
  await expect(win.locator('[data-role="impact"]')).toContainText('Co się stanie po zapisaniu: Ten opis zobaczy');
  await page.screenshot({ path: path.join(SHOTS, 'tp-ustawienia-zmien-opis.png') });
  await win.locator('[data-act="save"]').click();
  await expect(win).toHaveCount(0);
  await expect(s.locator('tf-alert[data-role="saved"]')).toHaveAttribute('title', 'Zapisano opis.');
  await expect(desc).toHaveText(text, { timeout: 15000 });
  await expect(about).toContainText(text);
  const detail = await busCall(page, 'busTopicDetailRequest', { instanceId, name: 'wyniki-badan' });
  expect(detail.topic.description).toBe(text);

  await s.locator('[data-go="change"][data-card="about"]').click();
  await win.locator('#tb-set-description textarea').fill('');
  await expect(win.locator('[data-role="impact"]')).toContainText('znów będzie widać rodzaj treści');
  await win.locator('[data-act="save"]').click();
  await expect(win).toHaveCount(0);
  await expect(desc).toHaveText('HL7 v2 · bez wzoru', { timeout: 15000 });
  await assertNoBannedWords(page);
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U2 Zapis i kopie: partitions only grow, an added partition gets a leader, confirmation and compression change', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openTopicPage(page, 'faktury', 'settings');
  const s = section(page, 'settings');
  await s.locator('[data-go="change"][data-card="write"]').click();
  const win = changeWindow(page);
  const partitions = win.locator('#tb-set-partitions input');
  await partitions.fill('1');
  await expect(win.locator('#tb-set-partitions')).toHaveAttribute('error', /od 2 do 256/);
  await expect(win.locator('[data-act="save"]')).toHaveAttribute('disabled', '');
  await partitions.fill('3');
  await expect(win.locator('[data-role="impact"]')).toContainText('Topik będzie miał 3 partycje.');
  await expect(win.locator('[data-role="impact"]')).toContainText('po ponownym połączeniu');
  await win.locator('#tb-set-compression .tf-seg-opt', { hasText: 'Wyłączona' }).click();
  await expect(win.locator('[data-role="impact"]')).toContainText('bez kompresji');
  await page.screenshot({ path: path.join(SHOTS, 'tp-ustawienia-zmien-zapis.png') });
  await win.locator('[data-act="save"]').click();
  await expect(win).toHaveCount(0);
  await expect(s.locator('tf-alert[data-role="saved"]')).toHaveAttribute('title', 'Zapisano zapis i kopie.');
  await expect(s.locator('.section-card[data-card="write"] .tb-vrow').first().locator('.tb-vr-value')).toHaveText('3', { timeout: 15000 });
  await expect(detailSlot(page).locator('[data-role="menu"] tf-tab#partitions')).toHaveAttribute('count', '3');
  // The added partition has a leader, so writes to it are not refused.
  await expect.poll(async () => {
    const r = await busCall(page, 'busReplicaListRequest', { instanceId, topic: 'faktury' });
    return (r?.partitions || []).find((p) => p.partition === 2)?.leaderNodeId || null;
  }, { timeout: 20000 }).not.toBeNull();
  // Back to compression on, so the seeded topic stays as it was apart from the partition.
  await s.locator('[data-go="change"][data-card="write"]').click();
  await expect(win.locator('#tb-set-partitions input')).toHaveValue('3');
  await win.locator('#tb-set-compression .tf-seg-opt', { hasText: 'Włączona' }).click();
  await win.locator('[data-act="save"]').click();
  await expect(win).toHaveCount(0);
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U2 Ponowne próby and Wzór wiadomości saved; a refusal stays in the window', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openTopicPage(page, 'wizyty', 'settings');
  const s = section(page, 'settings');
  const win = changeWindow(page);

  await s.locator('[data-go="change"][data-card="retry"]').click();
  await win.locator('#tb-set-attempts input').fill('3');
  await win.locator('#tb-set-backoff select').selectOption({ label: '5 s' });
  await expect(win.locator('[data-role="impact"]')).toContainText('po 3 próbach zamiast 5');
  await expect(win.locator('[data-role="impact"]')).toContainText('ok. 5 s i 10 s');
  await win.locator('[data-act="save"]').click();
  await expect(win).toHaveCount(0);
  await expect(s.locator('tf-alert[data-role="saved"]')).toHaveAttribute('title', 'Zapisano ponowne próby.');
  await expect(s.locator('.section-card[data-card="retry"]')).toContainText('3 razy');

  await s.locator('[data-go="change"][data-card="pattern"]').click();
  await expect(win.locator('#tb-set-schema select option')).toHaveText(['wizyta · JSON Schema', 'bez wzoru']);
  await win.locator('#tb-set-mode select').selectOption({ label: 'Przyjmij i zapisz ostrzeżenie' });
  await expect(win.locator('[data-role="impact"]')).toContainText('ostrzeżenie w swoim dzienniku');
  await page.screenshot({ path: path.join(SHOTS, 'tp-ustawienia-zmien-wzor.png') });
  await win.locator('[data-act="save"]').click();
  await expect(win).toHaveCount(0);
  await expect(s.locator('.section-card[data-card="pattern"]')).toContainText('przyjmij i zapisz ostrzeżenie', { timeout: 15000 });
  const detail = await busCall(page, 'busTopicDetailRequest', { instanceId, name: 'wizyty' });
  expect(detail.topic.validation).toBe('warn');
  expect(detail.topic.maxDeliveryAttempts).toBe(3);

  // The topic goes away while the window is open: the server's refusal is shown there.
  await busCall(page, 'busTopicCreateRequest', { instanceId, name: 'e2e-ustawienia', options: { partitions: 1 } });
  await openTopicPage(page, 'e2e-ustawienia', 'settings');
  await section(page, 'settings').locator('[data-go="change"][data-card="retry"]').click();
  await win.locator('#tb-set-attempts input').fill('2');
  await busCall(page, 'busTopicDeleteRequest', { instanceId, name: 'e2e-ustawienia' });
  await win.locator('[data-act="save"]').click();
  await expect(win.locator('[data-role="error"]')).toBeVisible({ timeout: 15000 });
  await expect(win.locator('[data-role="error"]')).toContainText(/e2e-ustawienia|topik/i);
  await expect(win).toHaveCount(1);
  await win.locator('[data-act="cancel"]').click();
  await expect(win).toHaveCount(0);
  expect(errors.filter((e) => !/topic_not_found|NotFound/.test(e)), errors.join('\n')).toEqual([]);
});

test('U2 delete from Ustawienia: the retyped name, then the list without the topic', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  await busCall(page, 'busTopicCreateRequest', { instanceId, name: 'e2e-do-usuniecia', options: { partitions: 1 } });
  await openTopicPage(page, 'e2e-do-usuniecia', 'settings');
  await section(page, 'settings').locator('.tb-danger-zone tf-button[data-go="delete"]').click();
  const win = page.locator('tf-window.tb-delete-window');
  await win.locator('#retype-input input').fill('e2e-do-usuniecia');
  await win.locator('tf-button[data-action="confirm"]').click();
  await expect(win).toHaveCount(0);
  await expect(tab(page, 'topics')).toHaveAttribute('aria-selected', 'true');
  await expect(topicsSlot(page).locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', 'Usunięto topik e2e-do-usuniecia.');
  await expect(topicRow(page, 'e2e-do-usuniecia')).toHaveCount(0);
  expect(hashParams(page).topic).toBeUndefined();
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U2 Partycje i kopie: one row per partition; on one node leadership cannot move and says why', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openTopicPage(page, 'wyniki-badan', 'partitions');
  const s = section(page, 'partitions');
  const table = s.locator('tf-table');
  await expect(table.locator('tbody tr')).toHaveCount(3, { timeout: 20000 });
  await expect(table.locator('tbody tr').first()).toContainText('Partycja 0');
  await expect(table.locator('tbody tr').first()).toContainText(/od \d[\d\s]* do \d/);
  const move = table.locator('tbody tr').first().locator('tf-button', { hasText: 'Przenieś prowadzenie' });
  await expect(move).toHaveAttribute('disabled', '', { timeout: 15000 });
  // A disabled button shows no tooltip: the reason is on the page, once for all partitions.
  await expect(s.locator('[data-role="blocked"] .tb-who-can')).toBeVisible();
  await expect(s.locator('[data-role="blocked"]')).toContainText('Partycja ma jedną kopię');
  await expect(s.locator('.tb-table-footer')).toContainText('kopia ma wszystkie wiadomości');
  await assertNoOverflow(page);
  await assertNoBannedWords(page);
  await page.screenshot({ path: path.join(SHOTS, 'tp-partycje.png'), fullPage: true });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U2 Kopie i nody: the node, nothing needing attention, partitions per topic lead to the topic', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openInstance(page, 'Produkcja');
  await tab(page, 'replication').click();
  const r = replicationSlot(page);
  const card = r.locator('.tb-node-card');
  await expect(card).toHaveCount(1, { timeout: 20000 });
  await expect(card.locator('[data-role="signal"]')).toHaveText('teraz (ten node)');
  await expect(card.locator('[data-role="sync"]')).toHaveText('9 z 9');
  await expect(r.locator('[data-role="nodes-sub"]')).toContainText('jeden node');
  await expect(r.locator('[data-role="attn-none"]')).toBeVisible();
  await expect(r.locator('[data-role="topics"] .topic-mini')).toHaveCount(3);
  await expect(r.locator('[data-role="topics"] .topic-mini[data-topic="faktury"]')).toContainText('3 partycje');
  await expect(r.locator('[data-role="changes-none"]')).toBeVisible();
  expect(await r.innerText()).not.toMatch(/Zmień repliki/);
  await assertNoBannedWords(page);
  await assertNoOverflow(page);
  await assertSentenceCaseChips(page);
  await page.screenshot({ path: path.join(SHOTS, 't10-kopie-i-nody.png'), fullPage: true });
  await r.locator('.topic-mini[data-topic="wizyty"]').click();
  await expect(detailSlot(page).locator('.tb-title')).toHaveText('wizyty', { timeout: 15000 });
  await expect(detailSlot(page).locator('[data-role="menu"]')).toHaveAttribute('value', 'partitions');
  expect(hashParams(page)).toMatchObject({ tab: 'topics', topic: 'wizyty', section: 'partitions' });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U2 at 390x844: the section list replaces the menu, cards fit, a window fills the phone', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(PHONE);
  await login(page);
  await openTopicPage(page, 'wyniki-badan');
  const d = detailSlot(page);
  await expect(d.locator('[data-role="menu"]')).toBeHidden();
  const pick = d.locator('[data-role="pick"]');
  await expect(pick).toBeVisible();
  await expect(section(page, 'state').locator('tf-stat-card')).toHaveCount(4, { timeout: 15000 });
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 'tp-stan-telefon.png'), fullPage: true });
  await pick.locator('select').selectOption('settings');
  await expect(section(page, 'settings')).toBeVisible();
  await expect.poll(() => hashParams(page).section).toBe('settings');
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 'tp-ustawienia-telefon.png'), fullPage: true });
  await section(page, 'settings').locator('[data-go="change"][data-card="retention"]').click();
  await expect(changeWindow(page)).toBeVisible();
  await windowFits(page, 'tf-window.tb-change-window', PHONE.width);
  await page.screenshot({ path: path.join(SHOTS, 'tp-ustawienia-zmien-przechowywanie-telefon.png') });
  await changeWindow(page).locator('[data-act="cancel"]').click();
  await pick.locator('select').selectOption('partitions');
  await expect(section(page, 'partitions').locator('tf-table tbody tr')).toHaveCount(3, { timeout: 15000 });
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 'tp-partycje-telefon.png'), fullPage: true });
  await tab(page, 'replication').click();
  await expect(replicationSlot(page).locator('.tb-node-card')).toHaveCount(1, { timeout: 15000 });
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 't10-kopie-i-nody-telefon.png'), fullPage: true });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U2 without administration: no change buttons, who can change, reading still works; without read access the sections close', async ({ page, browser }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  // A reader: bus.read on this instance only, and an explicit read deny on faktury.
  const users = await page.evaluate(async () => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return ApiBinary.one('iamListUsersRequest', {});
  });
  const list = users?.users || [];
  let reader = list.find((u) => u.username === 'tomasz');
  if (!reader) {
    const created = await page.evaluate(async () => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      return ApiBinary.action('iamCreateUserRequest', { username: 'tomasz', password: 'Tomasz-czyta-1', displayName: 'Tomasz Nowak', email: '', role: 'user', groupIds: [] });
    });
    reader = { userId: created?.userId ?? created?.user_id };
  }
  const readerId = reader.userId ?? reader.user_id ?? reader.id;
  expect(readerId).toBeTruthy();
  await page.evaluate(async ([addonId, subjectId]) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    await ApiBinary.action('addonPermissionSetRequest', { addonId, subjectType: 'user', subjectId, permissionId: 'bus.read', grantMode: 'allow' });
  }, [instanceId, readerId]);
  const admin = list.find((u) => u.username === 'admin');
  const adminId = admin?.userId ?? admin?.user_id ?? admin?.id ?? 'admin';
  await busCall(page, 'busAclSetRequest', { instanceId, topic: 'wyniki-badan', subjectType: 'user', subjectId: adminId, accessLevel: 'allow', action: 'admin' });
  await busCall(page, 'busAclSetRequest', { instanceId, topic: 'faktury', subjectType: 'user', subjectId: readerId, accessLevel: 'deny', action: 'read' });

  const context = await browser.newContext({ ignoreHTTPSErrors: true, locale: 'pl-PL', viewport: DESKTOP });
  const rp = await context.newPage();
  const readerErrors = trackErrors(rp);
  try {
    await rp.addInitScript(() => {
      localStorage.setItem('tentaflow_lang', 'pl');
      document.addEventListener('DOMContentLoaded', () => {
        const st = document.createElement('style');
        st.textContent = '.update-overlay{display:none!important}';
        document.head.appendChild(st);
      });
    });
    await loginAsAdmin(rp, { port: PORT, username: 'tomasz', password: 'Tomasz-czyta-1' });
    await rp.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wyniki-badan&section=settings`);
    const d = rp.locator('#tb-panel > [data-tb-view-slot="detail"]');
    const settings = d.locator('[data-section="settings"]');
    await expect(settings.locator('.section-card')).toHaveCount(5, { timeout: 20000 });
    await expect(settings.locator('tf-button')).toHaveCount(0);
    await expect(settings.locator('.tb-danger-zone')).toHaveCount(0);
    await expect(settings.locator('.tb-who-can')).toContainText('Zmiany w tym topiku może robić administrator topiku (');
    await expect(d.locator('[data-role="preview"]')).not.toHaveAttribute('disabled', '');
    await rp.screenshot({ path: path.join(SHOTS, 'tp-bez-uprawnien.png'), fullPage: true });
    await d.locator('[data-role="menu"] tf-tab#partitions > button').click();
    const table = d.locator('[data-section="partitions"] tf-table');
    await expect(table.locator('tbody tr')).toHaveCount(3, { timeout: 15000 });
    await expect(table.locator('tf-button')).toHaveCount(0);
    await expect(d.locator('[data-section="partitions"] .tb-who-can')).toBeVisible();

    await rp.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=faktury`);
    await expect(d.locator('.tb-title')).toHaveText('faktury', { timeout: 20000 });
    await expect(d.locator('[data-role="menu"] tf-tab#state')).toHaveAttribute('disabled', '');
    await expect(d.locator('[data-role="menu"] tf-tab#partitions')).toHaveAttribute('disabled', '');
    await expect(d.locator('[data-role="menu"]')).toHaveAttribute('value', 'settings');
    await expect(d.locator('[data-role="preview"]')).toHaveAttribute('disabled', '');
    await expect(d.locator('[data-role="preview-note"]')).toContainText('prawa czytania topiku faktury');
    await expect(d.locator('[data-section="settings"] .tb-who-can').first()).toContainText('Nie masz prawa czytania topiku faktury');
    await assertNoBannedWords(rp);
    await rp.screenshot({ path: path.join(SHOTS, 't12-bez-dostepu.png'), fullPage: true });

    await rp.locator('#tb-tabs tf-tab#replication > button').click();
    await expect(rp.locator('#tb-panel > [data-tb-view-slot="replication"] .tb-node-card')).toHaveCount(1, { timeout: 15000 });
    // A reader is not promised a leadership move they cannot make.
    await expect(rp.locator('#tb-panel > [data-tb-view-slot="replication"] [data-role="topics-sub"]')).toHaveText('Kliknij topik, aby zobaczyć jego partycje.');
    expect(readerErrors.filter((e) => !/PolicyDenied|permission_denied|protocol error/i.test(e)), readerErrors.join('\n')).toEqual([]);
  } finally {
    await context.close();
    await busCall(page, 'busAclSetRequest', { instanceId, topic: 'faktury', subjectType: 'user', subjectId: readerId, accessLevel: 'clear', action: 'read' });
  }
  expect(errors, errors.join('\n')).toEqual([]);
});

// ----------------------------------------------------------------------------
// U3 — Odbiorcy (T05) and a consumer's page (Stan, Miejsce czytania,
// Ustawienia). What the screen says is checked against the server itself: the
// consumer's row in the instance database and its reading places over the
// wire.
// ----------------------------------------------------------------------------

const moveWindow = (page) => page.locator('tf-window.tb-move-window');
const consumerSection = (page, id) => consumerSlot(page).locator(`.tb-section > [data-section="${id}"]`);

// The consumer's row as the node keeps it (the instance's own database).
function groupRow(group, topic) {
  for (const db of laggedSampleDbs()) {
    const out = execFileSync('/usr/bin/sqlite3', ['-separator', '|', db, `SELECT paused, commit_mode FROM bus_groups WHERE group_id = '${group}' AND topic = '${topic}';`], { encoding: 'utf8' }).trim();
    if (out) {
      const [paused, commitMode] = out.split('|');
      return { paused: paused === '1', commitMode };
    }
  }
  return null;
}

function auditCount(action) {
  return Number(execFileSync('/usr/bin/sqlite3', [DB, `SELECT COUNT(*) FROM audit_log WHERE action = '${action}';`], { encoding: 'utf8' }).trim());
}

async function readingPlaces(page, instanceId, group, topic) {
  const r = await busCall(page, 'busGroupDetailRequest', { instanceId, group, topic });
  return Object.fromEntries((r?.detail?.partitions || []).map((p) => [p.partition, { committed: p.committedOffset, waiting: p.lag, hw: p.committedOffset + p.lag }]));
}

async function openConsumerPage(page, group, topic, sectionId = null) {
  const instance = await openInstance(page, 'Produkcja');
  const params = new URLSearchParams({ instance, tab: 'groups', group, gtopic: topic });
  if (sectionId) params.set('section', sectionId);
  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?${params.toString()}`);
  await expect(consumerSlot(page).locator('.tb-title')).toHaveText(group, { timeout: 20000 });
  return instance;
}

// Grouped the way the screen prints it (a no-break space), so attributes compare exactly.
const fmt = (n) => new Intl.NumberFormat('pl-PL', { useGrouping: 'always' }).format(n);
// The Polish plural the screen picks for `n` (1 / 2–4 / the rest).
const plForm = (n, one, few, many) => {
  if (n === 1) return one;
  const d = n % 10;
  const h = n % 100;
  return d >= 2 && d <= 4 && !(h >= 12 && h <= 14) ? few : many;
};
const stale = (n) => `${fmt(n)} ${plForm(n, 'zaległą wiadomość', 'zaległe wiadomości', 'zaległych wiadomości')}`;

test('U3 Odbiorcy at 1440: the list, its filters and footer; pause in the row leads to the consumer, resume in its header', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openInstance(page, 'Produkcja');
  await tab(page, 'groups').click();
  const c = consumersSlot(page);
  const table = c.locator('[data-role="table"]');
  await expect(table.locator('tbody tr')).toHaveCount(4, { timeout: 15000 });
  await expect(c.locator('[data-role="filter"] .tf-seg-opt')).toHaveText(['Wszyscy 4', 'Opóźnieni 3', 'Wstrzymani 1']);
  const row = (name) => table.locator('tbody tr', { hasText: name });
  await expect(row('aplikacja-lekarza')).toContainText('wyniki-badan');
  await expect(row('aplikacja-lekarza')).toContainText('po udanym przetworzeniu');
  await expect(row('aplikacja-lekarza')).toContainText('działa');
  await expect(row('system-rozliczen')).toContainText('wstrzymany');
  await expect(row('system-rozliczen').locator('tf-button[data-act="resume"]')).toHaveText('Wznów');
  await expect(row('raporty-laboratorium').locator('tf-button[data-act="pause"]')).toHaveText('Wstrzymaj');
  await expect(c.locator('[data-role="footer"]')).toContainText('4 odbiorcy');
  await expect(c.locator('[data-role="footer"]')).toContainText('wstrzymanych: 1');
  await expect(c.locator('.tb-commit-legend .legend-item')).toHaveCount(3);
  // Search by the topic, then the Opóźnieni filter: nothing waiting is not behind.
  await c.locator('[data-role="search"] input').fill('faktury');
  await expect(table.locator('tbody tr')).toHaveCount(1);
  await c.locator('[data-role="search"] input').fill('');
  await expect(table.locator('tbody tr')).toHaveCount(4);
  await c.locator('[data-role="filter"] .tf-seg-opt', { hasText: 'Opóźnieni' }).click();
  await expect(table.locator('tbody tr')).toHaveCount(3);
  await expect(row('raporty-laboratorium')).toHaveCount(0);
  await c.locator('[data-role="filter"] .tf-seg-opt', { hasText: 'Wszyscy' }).click();
  await assertNoOverflow(page);
  await assertNoBannedWords(page);
  await assertSentenceCaseChips(page);
  await page.screenshot({ path: path.join(SHOTS, 't05-odbiorcy.png'), fullPage: true });

  // "Wstrzymaj" in the row: the server pauses it, the consumer's Stan says so.
  await row('raporty-laboratorium').locator('tf-button[data-act="pause"]').click();
  const d = consumerSlot(page);
  await expect(d.locator('.tb-title')).toHaveText('raporty-laboratorium', { timeout: 15000 });
  await expect(consumerSection(page, 'state').locator('tf-alert')).toHaveAttribute('title', 'Wstrzymano odbiorcę.');
  await expect(consumerSection(page, 'state').locator('tf-alert')).toHaveAttribute('message', /raporty-laboratorium nie dostają nowych wiadomości z topiku wyniki-badan/);
  await expect(consumerSection(page, 'state').locator('tf-stat-card[data-kpi="state"]')).toHaveAttribute('value', 'wstrzymany', { timeout: 15000 });
  await expect(d.locator('[data-role="toggle"]')).toHaveText('Wznów');
  expect(groupRow('raporty-laboratorium', 'wyniki-badan').paused).toBe(true);
  await page.screenshot({ path: path.join(SHOTS, 'od-wstrzymany.png'), fullPage: true });

  // The list follows: two paused now.
  await d.locator('[data-go="back"]').click();
  await expect(c.locator('[data-role="filter"] .tf-seg-opt')).toHaveText(['Wszyscy 4', 'Opóźnieni 3', 'Wstrzymani 2'], { timeout: 15000 });
  await c.locator('[data-role="filter"] .tf-seg-opt', { hasText: 'Wstrzymani' }).click();
  await expect(table.locator('tbody tr')).toHaveCount(2);
  await row('raporty-laboratorium').click();

  // "Wznów" in the page header.
  await d.locator('[data-role="toggle"]').click();
  await expect(consumerSection(page, 'state').locator('tf-alert')).toHaveAttribute('title', 'Wznowiono odbiorcę.', { timeout: 15000 });
  await expect(consumerSection(page, 'state').locator('tf-stat-card[data-kpi="state"]')).toHaveAttribute('value', 'działa', { timeout: 15000 });
  await expect(d.locator('[data-role="toggle"]')).toHaveText('Wstrzymaj');
  expect(groupRow('raporty-laboratorium', 'wyniki-badan').paused).toBe(false);
  await page.screenshot({ path: path.join(SHOTS, 'od-wznowiony.png'), fullPage: true });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U3 consumer page: Stan, Miejsce czytania and Ustawienia, each alone; the way of confirming is what the program chose', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openConsumerPage(page, 'aplikacja-lekarza', 'wyniki-badan');
  const d = consumerSlot(page);
  await expect(d.locator('[data-role="desc"]')).toHaveText('czyta topik wyniki-badan');
  await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(['TentaBus', 'Produkcja', 'Odbiorcy', 'aplikacja-lekarza']);
  const menu = d.locator('[data-role="menu"]');
  await expect(menu).toHaveAttribute('orientation', 'vertical');
  await expect(menu.locator('tf-tab')).toHaveText([/Stan/, /Miejsce czytania\s*3/, /Ustawienia/]);

  const places = await readingPlaces(page, instanceId, 'aplikacja-lekarza', 'wyniki-badan');
  const total = Object.values(places).reduce((sum, p) => sum + p.waiting, 0);
  const s = consumerSection(page, 'state');
  const tile = (k) => s.locator(`tf-stat-card[data-kpi="${k}"]`);
  await expect(tile('waiting')).toHaveAttribute('value', fmt(total), { timeout: 15000 });
  // Nobody consumes during the run: the backlog waits, it does not grow.
  await expect(tile('waiting')).toHaveAttribute('delta', /^czeka od \d+ (min|godz)/);
  await expect(tile('rate')).toHaveAttribute('delta', /do topiku przybywa \d+\/s|za mało pomiarów/);
  await expect(tile('state')).toHaveAttribute('value', 'działa');
  await expect(tile('state')).toHaveAttribute('delta', 'potwierdza po udanym przetworzeniu');
  await expect(tile('dlq')).toHaveAttribute('value', '14');
  await expect(tile('dlq')).toHaveAttribute('delta', '14 w ostatniej godzinie');
  await expect(s.locator('.tb-alert [data-role="title"]')).toHaveText(['Odbiorca nie nadąża', '14 nieprzetworzonych wiadomości tego odbiorcy']);
  await expect(s.locator('[data-role="topic-sub"]')).toHaveText('HL7 v2 · 3 partycje');
  await assertNoOverflow(page);
  await assertNoBannedWords(page);
  await page.screenshot({ path: path.join(SHOTS, 'od-stan.png'), fullPage: true });

  // Miejsce czytania: the last message read, the newest one and what waits, as the server has them.
  await s.locator('.tb-alert', { hasText: 'nie nadąża' }).locator('tf-button').click();
  await expect.poll(() => hashParams(page).section).toBe('position');
  const ps = consumerSection(page, 'position');
  await expect(s).toBeHidden();
  const rows = ps.locator('tf-table tbody tr');
  await expect(rows).toHaveCount(3, { timeout: 15000 });
  for (const [p, v] of Object.entries(places)) {
    const r = rows.nth(Number(p));
    await expect(r).toContainText(`Partycja ${p}`);
    await expect(r).toContainText(v.committed > 0 ? fmt(v.committed - 1) : 'nic');
    await expect(r).toContainText(fmt(v.hw - 1));
    await expect(r).toContainText(fmt(v.waiting));
    await expect(r.locator('tf-button', { hasText: 'Przesuń' })).toBeVisible();
  }
  await expect(ps.locator('[data-role="footer"]')).toContainText(`Razem czeka ${fmt(total)} wiadomości.`);
  await page.screenshot({ path: path.join(SHOTS, 'od-miejsce-czytania.png'), fullPage: true });

  // Ustawienia: values only, the way of confirming locked, retries lead to the topic.
  await menu.locator('tf-tab#settings > button').click();
  const st = consumerSection(page, 'settings');
  await expect(st.locator('.section-card')).toHaveCount(2);
  await expect(st.locator('[data-go="change"]')).toHaveCount(0);
  await expect(st).toContainText('po udanym przetworzeniu');
  await expect(st).toContainText('Ustawia go program odbiorcy przy każdym połączeniu');
  await expect(st).toContainText(/\d+ prób, pierwsza przerwa/);
  expect(await st.innerText()).not.toMatch(/czas na odpowiedź/i);
  await page.screenshot({ path: path.join(SHOTS, 'od-ustawienia.png'), fullPage: true });
  // The page polls every few seconds: the program's own choice must survive it (§0.1).
  await page.waitForTimeout(10_000);
  expect(groupRow('aplikacja-lekarza', 'wyniki-badan').commitMode).toBe('auto_after_success');
  await expect(st).toContainText('po udanym przetworzeniu');
  await st.locator('[data-go="topic-settings"]').click();
  await expect(detailSlot(page).locator('.tb-title')).toHaveText('wyniki-badan', { timeout: 15000 });
  await expect(detailSlot(page).locator('[data-role="menu"]')).toHaveAttribute('value', 'settings');
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U3 Przesuń: to a number, to the start, to the end and to a time — the window counts what the server then does', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const group = 'rejestracja-online';
  const topic = 'wizyty';
  const instanceId = await openConsumerPage(page, group, topic, 'position');
  const ps = consumerSection(page, 'position');
  const rows = ps.locator('tf-table tbody tr');
  await expect(rows).toHaveCount(3, { timeout: 15000 });
  const audits = auditCount('bus.offset.reset');
  const topicDetail = await busCall(page, 'busTopicDetailRequest', { instanceId, name: topic });
  const earliest = Object.fromEntries(topicDetail.partitions.map((p) => [p.partition, p.earliestOffset]));
  const win = moveWindow(page);
  const impact = win.locator('[data-role="impact"]');
  const moveButton = (p) => rows.nth(p).locator('tf-button', { hasText: 'Przesuń' });

  // 1. A chosen number: five messages back on partition 0.
  let before = await readingPlaces(page, instanceId, group, topic);
  await moveButton(0).click();
  await expect(win.locator('.tf-window-title-text')).toHaveText(`Przesuń miejsce czytania — ${group}, partycja 0`);
  await expect(win.locator('[data-role="now"]')).toHaveText(`przeczytano do numeru ${fmt(before[0].committed - 1)} z ${fmt(before[0].hw - 1)}`);
  const target = before[0].committed - 5;
  await win.locator('#tb-move-offset input').fill(String(target));
  await expect(impact).toContainText(`odbiorca ${group} przeczyta ponownie 5 wiadomości z partycji 0 (od numeru ${fmt(target)}); w tej partycji czekać będzie ${fmt(before[0].waiting + 5)}.`);
  await expect(impact).toContainText('przejdzie na nowe miejsce przy następnym pobraniu');
  await page.screenshot({ path: path.join(SHOTS, 'od-przesun-p0.png') });
  await win.locator('[data-act="move"]').click();
  await expect(win).toHaveCount(0);
  await expect(ps.locator('tf-alert')).toHaveAttribute('title', 'Zapisano nowe miejsce czytania.');
  let after = await readingPlaces(page, instanceId, group, topic);
  await expect(ps.locator('tf-alert')).toHaveAttribute('message', `Partycja 0: odbiorca czyta od numeru ${fmt(target)}; w tej partycji czeka ${fmt(after[0].waiting)} ${plForm(after[0].waiting, 'wiadomość', 'wiadomości', 'wiadomości')}.`);
  expect(after[0].committed).toBe(target);
  await expect(rows.nth(0)).toContainText('zmieniono przed chwilą', { timeout: 15000 });
  await expect(moveButton(0)).toHaveAttribute('disabled', '');
  await expect(rows.nth(0)).toContainText(fmt(after[0].waiting));
  await page.screenshot({ path: path.join(SHOTS, 'od-przesunieto-p0.png'), fullPage: true });

  // A number the topic does not keep is refused in the window; Anuluj changes nothing.
  await moveButton(1).click();
  await win.locator('#tb-move-offset input').fill(String(after[1].hw));
  await expect(win.locator('#tb-move-offset')).toHaveAttribute('error', /^Podaj numer od /);
  await expect(win.locator('[data-act="move"]')).toHaveAttribute('disabled', '');
  await win.locator('[data-act="cancel"]').click();
  await expect(win).toHaveCount(0);
  expect((await readingPlaces(page, instanceId, group, topic))[1].committed).toBe(after[1].committed);

  // 2. The oldest kept message, on partition 1.
  before = after;
  await moveButton(1).click();
  await win.locator('tf-choice-card[value="earliest"]').click();
  const back1 = before[1].committed - earliest[1];
  await expect(impact).toContainText(`przeczyta ponownie ${fmt(back1)} ${back1 === 1 ? 'wiadomość' : 'wiadomości'} z partycji 1 (od numeru ${fmt(earliest[1])})`);
  await win.locator('[data-act="move"]').click();
  await expect(win).toHaveCount(0);
  after = await readingPlaces(page, instanceId, group, topic);
  expect(after[1].committed).toBe(earliest[1]);

  // 3. The newest message, on partition 2: every waiting one is skipped.
  before = after;
  await moveButton(2).click();
  await win.locator('tf-choice-card[value="latest"]').click();
  await expect(impact).toContainText(`odbiorca ${group} pominie ${stale(before[2].waiting)} z partycji 2 i zacznie od nowych.`);
  await win.locator('[data-act="move"]').click();
  await expect(win).toHaveCount(0);
  await expect(ps.locator('tf-alert')).toHaveAttribute('message', `Partycja 2: odbiorca czyta od numeru ${fmt(before[2].hw)}; w tej partycji czeka 0 wiadomości.`);
  after = await readingPlaces(page, instanceId, group, topic);
  expect(after[2].committed).toBe(before[2].hw);
  expect(after[2].waiting).toBe(0);

  // 4. A chosen time, on partition 0 again after "Odśwież": first a time after
  // every message (nothing to read again), then one before all of them.
  await page.locator('#tb-refresh').click();
  await expect(moveButton(0)).not.toHaveAttribute('disabled', '', { timeout: 15000 });
  before = await readingPlaces(page, instanceId, group, topic);
  await moveButton(0).click();
  await win.locator('tf-choice-card[value="timestamp"]').click();
  await expect(impact).toContainText('Wybierz datę i godzinę');
  await win.locator('#tb-move-time input').fill('2099-01-01T00:00');
  await expect(impact).toContainText(`pominie ${stale(before[0].waiting)} z partycji 0 i zacznie od nowych.`, { timeout: 15000 });
  await expect(impact).toContainText('także zapisane przed tą godziną');
  await win.locator('#tb-move-time input').fill('2020-01-01T00:00');
  const back0 = before[0].committed - earliest[0];
  await expect(impact).toContainText(`przeczyta ponownie ${fmt(back0)} ${plForm(back0, 'wiadomość', 'wiadomości', 'wiadomości')} z partycji 0 (od numeru ${fmt(earliest[0])})`, { timeout: 15000 });
  await page.screenshot({ path: path.join(SHOTS, 'od-przesun-chwila.png') });
  await win.locator('[data-act="move"]').click();
  await expect(win).toHaveCount(0);
  after = await readingPlaces(page, instanceId, group, topic);
  expect(after[0].committed).toBe(earliest[0]);

  // Every move is in the audit log.
  expect(auditCount('bus.offset.reset')).toBe(audits + 4);
  // Stan's waiting count follows the moves.
  const total = Object.values(after).reduce((sum, p) => sum + p.waiting, 0);
  await consumerSlot(page).locator('[data-role="menu"] tf-tab#state > button').click();
  await expect(consumerSection(page, 'state').locator('tf-stat-card[data-kpi="waiting"]')).toHaveAttribute('value', fmt(total), { timeout: 15000 });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U3 at 390x844: the list as cards, the section list, the move window fills the phone', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(PHONE);
  await login(page);
  await openInstance(page, 'Produkcja');
  await tab(page, 'groups').click();
  await expect(consumersSlot(page).locator('[data-role="table"] tbody tr')).toHaveCount(4, { timeout: 15000 });
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 't05-odbiorcy-telefon.png'), fullPage: true });
  await consumersSlot(page).locator('[data-role="table"] tbody tr', { hasText: 'aplikacja-lekarza' }).click();
  const d = consumerSlot(page);
  await expect(d.locator('.tb-title')).toHaveText('aplikacja-lekarza', { timeout: 15000 });
  await expect(d.locator('[data-role="menu"]')).toBeHidden();
  const pick = d.locator('[data-role="pick"]');
  await expect(pick).toBeVisible();
  await expect(consumerSection(page, 'state').locator('tf-stat-card')).toHaveCount(4, { timeout: 15000 });
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 'od-stan-telefon.png'), fullPage: true });
  await pick.locator('select').selectOption('position');
  await expect(consumerSection(page, 'position').locator('tf-table tbody tr')).toHaveCount(3, { timeout: 15000 });
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 'od-miejsce-czytania-telefon.png'), fullPage: true });
  await consumerSection(page, 'position').locator('tf-table tbody tr').first().locator('tf-button', { hasText: 'Przesuń' }).click();
  await expect(moveWindow(page).locator('.tf-window-title-text')).toBeVisible();
  await windowFits(page, 'tf-window.tb-move-window', PHONE.width);
  await page.screenshot({ path: path.join(SHOTS, 'od-przesun-p0-telefon.png') });
  await moveWindow(page).locator('[data-act="cancel"]').click();
  await pick.locator('select').selectOption('settings');
  await expect(consumerSection(page, 'settings').locator('.section-card')).toHaveCount(2);
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 'od-ustawienia-telefon.png'), fullPage: true });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U3 without rights: no pause or move, who can; a consumer of a topic the reader may not read is not shown at all', async ({ page, browser }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  const users = await page.evaluate(async () => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return ApiBinary.one('iamListUsersRequest', {});
  });
  const reader = (users?.users || []).find((u) => u.username === 'tomasz');
  expect(reader, 'the reader created by the U2 test').toBeTruthy();
  const readerId = reader.userId ?? reader.user_id ?? reader.id;
  await busCall(page, 'busAclSetRequest', { instanceId, topic: 'faktury', subjectType: 'user', subjectId: readerId, accessLevel: 'deny', action: 'read' });

  const context = await browser.newContext({ ignoreHTTPSErrors: true, locale: 'pl-PL', viewport: DESKTOP });
  const rp = await context.newPage();
  const readerErrors = trackErrors(rp);
  try {
    await rp.addInitScript(() => {
      localStorage.setItem('tentaflow_lang', 'pl');
      document.addEventListener('DOMContentLoaded', () => {
        const st = document.createElement('style');
        st.textContent = '.update-overlay{display:none!important}';
        document.head.appendChild(st);
      });
    });
    await loginAsAdmin(rp, { port: PORT, username: 'tomasz', password: 'Tomasz-czyta-1' });
    await rp.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=groups`);
    const list = consumersSlot(rp);
    const table = list.locator('[data-role="table"]');
    // system-rozliczen reads faktury, which this reader may not read: not listed, not counted.
    await expect(table.locator('tbody tr')).toHaveCount(3, { timeout: 20000 });
    await expect(table.locator('tbody tr', { hasText: 'system-rozliczen' })).toHaveCount(0);
    await expect(rp.locator('#tb-tabs tf-tab#groups')).toHaveAttribute('count', '3', { timeout: 15000 });
    await expect(table.locator('tf-button[data-act="pause"], tf-button[data-act="resume"]')).toHaveCount(0);
    await expect(list.locator('[data-role="admin-note"]')).toBeVisible();
    await expect(list.locator('[data-role="admin-note"]')).toContainText('administrator topiku');
    await rp.screenshot({ path: path.join(SHOTS, 't05-bez-uprawnien.png'), fullPage: true });

    await table.locator('tbody tr', { hasText: 'aplikacja-lekarza' }).click();
    const d = consumerSlot(rp);
    await expect(d.locator('.tb-title')).toHaveText('aplikacja-lekarza', { timeout: 15000 });
    await expect(d.locator('[data-role="toggle"]')).toHaveCount(0);
    await expect(d.locator('.tb-title-note')).toContainText('Wstrzymywać tego odbiorcę i przesuwać jego miejsce czytania może administrator topiku wyniki-badan (');
    await d.locator('[data-role="menu"] tf-tab#position > button').click();
    const ps = d.locator('[data-section="position"]');
    await expect(ps.locator('tf-table tbody tr')).toHaveCount(3, { timeout: 15000 });
    await expect(ps.locator('tf-table tf-button')).toHaveCount(0);
    await expect(ps.locator('.tb-who-can')).toHaveCount(0);
    await rp.screenshot({ path: path.join(SHOTS, 'od-bez-uprawnien.png'), fullPage: true });

    // A link to the hidden consumer says it is not there, without its data.
    await rp.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=groups&group=system-rozliczen&gtopic=faktury`);
    await expect(d.locator('tf-empty-state')).toHaveAttribute('title', 'Odbiorcy system-rozliczen już nie ma', { timeout: 20000 });
    await assertNoBannedWords(rp);
    expect(readerErrors.filter((e) => !/PolicyDenied|permission_denied|group_not_found|NotFound|protocol error/i.test(e)), readerErrors.join('\n')).toEqual([]);
  } finally {
    await context.close();
    await busCall(page, 'busAclSetRequest', { instanceId, topic: 'faktury', subjectType: 'user', subjectId: readerId, accessLevel: 'clear', action: 'read' });
  }
  expect(errors, errors.join('\n')).toEqual([]);
});

// ----------------------------------------------------------------------------
// U4 — Nieprzetworzone wiadomości (T06) and a topic's section. The seed leaves
// 14 messages aplikacja-lekarza gave up on in wyniki-badan (HL7) and three in
// wizyty (JSON): two rejestracja-online gave up on and one the pattern
// rejected at write. What the screen says after each change is checked
// against the server: the topic's own messages, the audit log, the counters.
// ----------------------------------------------------------------------------

const unpSlot = (page) => page.locator('#tb-panel > [data-tb-view-slot="dlq"]');
const unpTile = (page, topic) => unpSlot(page).locator(`.tb-unp-tile[data-topic="${topic}"]`);
const unpTable = (page) => section(page, 'dlq').locator('[data-role="table"]');
const unpWindow = (page, cls) => page.locator(`tf-window.${cls}`);

async function openUnprocessed(page) {
  const instance = await openInstance(page, 'Produkcja');
  await tab(page, 'dlq').click();
  await expect(unpSlot(page).locator('.tb-unp-tile')).toHaveCount(2, { timeout: 20000 });
  return instance;
}

// How many messages of `topic` the server lists (every page) and the next
// number of the topic itself — where a retried message lands.
async function serverUnprocessed(page, instanceId, topic) {
  const r = await busCall(page, 'busDlqListRequest', { instanceId, sourceTopic: topic, limit: 100, newestFirst: true });
  return r.records || [];
}
async function topicEnd(page, instanceId, topic) {
  const d = await busCall(page, 'busTopicDetailRequest', { instanceId, name: topic });
  return (d.partitions || []).reduce((s, p) => s + Number(p.highWatermark), 0);
}

// The same number of unprocessed messages in the header card, the main tab
// and — on a topic's page — its section menu.
async function expectCounters(page, total, topicCount = null) {
  await expect(page.locator('.tb-app-head [data-role="dlq"]')).toHaveAttribute('label', new RegExp(`^${total} `), { timeout: 15000 });
  await expect(page.locator('#tb-tabs tf-tab#dlq')).toHaveAttribute('count', String(total), { timeout: 15000 });
  if (topicCount != null) {
    const menuTab = detailSlot(page).locator('[data-role="menu"] tf-tab#dlq');
    if (topicCount === 0) await expect(menuTab).not.toHaveAttribute('count', /.+/, { timeout: 15000 });
    else await expect(menuTab).toHaveAttribute('count', String(topicCount), { timeout: 15000 });
  }
}

const rowText = async (table) => (await table.locator('tbody tr').allTextContents()).map(norm);

test('U4 Nieprzetworzone at 1440: a tile per topic, one list newest first, a row leads to its topic', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openUnprocessed(page);
  const u = unpSlot(page);
  await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(['TentaBus', 'Produkcja', 'Nieprzetworzone']);
  await expect(u.locator('.tb-unp-tile')).toHaveText([/wyniki-badan/, /wizyty/]);
  await expect(unpTile(page, 'wyniki-badan').locator('[data-role="count"]')).toHaveAttribute('label', '14');
  await expect(unpTile(page, 'wizyty').locator('[data-role="count"]')).toHaveAttribute('label', '3');
  // Each tile's chip is its own count; the list header has the instance total.
  await expect(unpTile(page, 'wyniki-badan').locator('[data-role="count"] tf-chip')).toHaveCount(0);
  await expect(u.locator('[data-role="list-count"] tf-chip')).toHaveAttribute('label', '17');
  await expect(unpTile(page, 'wyniki-badan').locator('[data-role="sub"]')).toHaveText(/^najczęściej: program odbiorcy zgłosił błąd · ostatnia dziś \d\d:\d\d$/, { timeout: 15000 });
  // The one rejected at write is not retried with the rest.
  await expect(unpTile(page, 'wyniki-badan').locator('[data-role="retry-all"]')).toHaveText('Ponów wszystkie (14)');
  await expect(unpTile(page, 'wizyty').locator('[data-role="retry-all"]')).toHaveText('Ponów wszystkie (2)');

  const table = u.locator('[data-role="table"]');
  await expect(table.locator('tbody tr')).toHaveCount(10, { timeout: 15000 });
  await expect(u.locator('[data-role="footer"]')).toHaveText(/Pokazano 10 z 17/);
  // wizyty's failed last: newest first puts all three on top.
  const rows = await rowText(table);
  expect(rows.slice(0, 3).every((r) => r.includes('wizyty'))).toBeTruthy();
  const atWrite = rows.find((r) => r.includes('przy zapisie'));
  expect(atWrite).toContain('nie trafiła do topiku');
  expect(atWrite).toContain('Nie pasuje do wzoru wiadomości');
  expect(atWrite).toContain('1 z 1');
  expect(rows.filter((r) => r.includes('rejestracja-online')).every((r) => r.includes('5 z 5'))).toBeTruthy();
  await u.locator('[data-role="more"]').click();
  await expect(table.locator('tbody tr')).toHaveCount(17);
  await expect(u.locator('[data-role="more"]')).toBeHidden();
  await expect(u.locator('[data-role="footer"]')).toHaveText(/Pokazano 17 z 17/);
  await assertNoOverflow(page);
  await assertNoBannedWords(page);
  await assertNoInternalTopics(page);
  await assertSentenceCaseChips(page);
  await page.screenshot({ path: path.join(SHOTS, 't06-nieprzetworzone.png'), fullPage: true });

  // A row and a tile lead to the section of their topic.
  await table.locator('tbody tr', { hasText: 'przy zapisie' }).click();
  await expect(tab(page, 'topics')).toHaveAttribute('aria-selected', 'true');
  await expect.poll(() => hashParams(page)).toMatchObject({ tab: 'topics', topic: 'wizyty', section: 'dlq' });
  await expect(unpTable(page).locator('tbody tr')).toHaveCount(3, { timeout: 15000 });
  await expect(page.locator('#tb-crumbs .tf-breadcrumb-item')).toHaveText(['TentaBus', 'Produkcja', 'Topiki', 'wizyty']);
  await tab(page, 'dlq').click();
  await unpTile(page, 'wyniki-badan').click();
  await expect.poll(() => hashParams(page)).toMatchObject({ topic: 'wyniki-badan', section: 'dlq' });
  await expect(unpTable(page).locator('tbody tr')).toHaveCount(10, { timeout: 15000 });
  await expect(section(page, 'dlq').locator('[data-role="footer"]')).toHaveText(/Pokazano 10 z 14/);
  await expectCounters(page, 17, 14);
  await page.screenshot({ path: path.join(SHOTS, 'tp-nieprzetworzone.png'), fullPage: true });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U4 Pokaż: the failure in words and the body with the topic\'s hidden fields hidden (JSON and HL7)', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  const hide = (topic, fields) => busCall(page, 'busFieldPolicySetRequest', { instanceId, topic, subjectType: 'any', subjectId: '*', direction: 'read', fields, requiredFields: [] });
  await hide('wizyty', ['termin', 'lekarz']);
  await hide('wyniki-badan', ['MSH-9', 'PID-3', 'OBX-5']);
  try {
    const audits = auditCount('bus.messages.browse');
    await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wizyty&section=dlq`);
    const table = unpTable(page);
    await expect(table.locator('tbody tr')).toHaveCount(3, { timeout: 20000 });
    expect(auditCount('bus.messages.browse')).toBeGreaterThan(audits);
    const failed = table.locator('tbody tr', { hasText: 'rejestracja-online' }).first();
    await failed.locator('tf-button[data-act="view"]').click();
    const win = unpWindow(page, 'tb-unp-view');
    await expect(win).toHaveCount(1);
    await expect(win).toContainText('Program odbiorcy zgłosił błąd');
    // The consumer's error text may quote a hidden value; under a hiding rule it is not shown.
    await expect(win).toContainText(/Opis błędu\s*ukryty przez zasady ukrywania danych tego topiku/);
    await expect(win).not.toContainText('termin jest już zajęty');
    await expect(win).toContainText(/Próby\s*5 z 5/);
    await expect(win).toContainText('Obowiązują tu te same zasady ukrywania danych');
    const body = await win.locator('[data-role="payload"]').textContent();
    expect(body).toContain('termin');
    expect(body).not.toContain('pacjent');
    expect(body).not.toContain('P-0');
    await windowFits(page, 'tf-window.tb-unp-view', DESKTOP.width);
    await page.screenshot({ path: path.join(SHOTS, 'tp-nieprzetworzone-pokaz.png') });
    await win.locator('[data-act="close"]').click();
    await expect(win).toHaveCount(0);

    // The HL7 messages are read as HL7 with wyniki-badan's rule, never as an empty "{}".
    await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wyniki-badan&section=dlq`);
    await expect(table.locator('tbody tr')).toHaveCount(10, { timeout: 20000 });
    await table.locator('tbody tr').first().locator('tf-button[data-act="view"]').click();
    const hl7 = await win.locator('[data-role="payload"]').textContent();
    expect(hl7).toMatch(/^MSH\|/);
    expect(hl7).toContain('ORU^R01');
    expect(hl7).toMatch(/PID\|\|\|8001011\d{4}\|/);
    expect(hl7).not.toContain('Kowalski');
    await win.locator('[data-act="close"]').click();
  } finally {
    for (const topic of ['wizyty', 'wyniki-badan']) {
      await busCall(page, 'busFieldPolicyDeleteRequest', { instanceId, topic, subjectType: 'any', subjectId: '*', direction: 'read' });
    }
  }
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U4 at 390x844: tiles and the list as cards, the section list, a window fills the phone', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(PHONE);
  await login(page);
  const instanceId = await openUnprocessed(page);
  await expect(unpSlot(page).locator('[data-role="table"] tbody tr').first()).toBeVisible({ timeout: 15000 });
  await assertNoOverflow(page);
  const button = unpTile(page, 'wyniki-badan').locator('[data-role="retry-all"]');
  const box = await button.boundingBox();
  expect(box.x + box.width).toBeLessThanOrEqual(PHONE.width);
  await page.screenshot({ path: path.join(SHOTS, 't06-nieprzetworzone-telefon.png'), fullPage: true });

  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wizyty&section=dlq`);
  await expect(detailSlot(page).locator('[data-role="pick"] select')).toHaveValue('dlq', { timeout: 20000 });
  const table = unpTable(page);
  await expect(table.locator('tbody tr')).toHaveCount(3, { timeout: 15000 });
  const buttons = table.locator('tf-button');
  const count = await buttons.count();
  expect(count).toBe(8);
  for (let i = 0; i < count; i += 1) {
    const b = buttons.nth(i);
    await b.scrollIntoViewIfNeeded();
    const r = await b.boundingBox();
    expect(r.x).toBeGreaterThanOrEqual(0);
    expect(r.x + r.width).toBeLessThanOrEqual(PHONE.width);
  }
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 'tp-nieprzetworzone-telefon.png'), fullPage: true });
  await buttons.filter({ hasText: 'Pokaż' }).first().click();
  await expect(unpWindow(page, 'tb-unp-view')).toHaveCount(1);
  await windowFits(page, 'tf-window.tb-unp-view', PHONE.width);
  await page.screenshot({ path: path.join(SHOTS, 'tp-nieprzetworzone-pokaz-telefon.png') });
  await unpWindow(page, 'tb-unp-view').locator('[data-act="close"]').click();
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U4 without rights: counts and the messages to read, no retry or discard, who can; the server refuses them too', async ({ page, browser }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  const context = await browser.newContext({ ignoreHTTPSErrors: true, locale: 'pl-PL', viewport: DESKTOP });
  const rp = await context.newPage();
  const readerErrors = trackErrors(rp);
  try {
    await rp.addInitScript(() => {
      localStorage.setItem('tentaflow_lang', 'pl');
      document.addEventListener('DOMContentLoaded', () => {
        const st = document.createElement('style');
        st.textContent = '.update-overlay{display:none!important}';
        document.head.appendChild(st);
      });
    });
    await loginAsAdmin(rp, { port: PORT, username: 'tomasz', password: 'Tomasz-czyta-1' });
    await rp.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=dlq`);
    await expect(unpSlot(rp).locator('.tb-unp-tile')).toHaveCount(2, { timeout: 20000 });
    await expect(unpSlot(rp).locator('[data-role="table"] tbody tr')).toHaveCount(10, { timeout: 15000 });
    await expect(unpSlot(rp).locator('[data-role="retry-all"]:visible')).toHaveCount(0);
    await expect(unpSlot(rp).locator('[data-role="list-hint"]')).toHaveText('Kliknij wiersz, aby przejść do nieprzetworzonych wiadomości jego topiku.');
    await expect(unpTile(rp, 'wyniki-badan').locator('[data-role="who"]')).toContainText('Ponawiać i odrzucać wiadomości może administrator topiku (', { timeout: 15000 });
    await rp.screenshot({ path: path.join(SHOTS, 't06-bez-uprawnien.png'), fullPage: true });

    await unpTile(rp, 'wyniki-badan').click();
    const d = rp.locator('#tb-panel > [data-tb-view-slot="detail"]');
    const s = d.locator('[data-section="dlq"]');
    await expect(s.locator('tf-table tbody tr')).toHaveCount(10, { timeout: 15000 });
    await expect(s.locator('[data-go="unp-retry-all"]')).toHaveCount(0);
    const acts = await s.locator('tf-table tf-button').evaluateAll((els) => [...new Set(els.map((b) => b.dataset.act))]);
    expect(acts).toEqual(['view']);
    await expect(s.locator('.tb-who-can')).toContainText('Ponawiać i odrzucać wiadomości może administrator topiku (');
    await s.locator('tf-table tf-button[data-act="view"]').first().click();
    const win = unpWindow(rp, 'tb-unp-view');
    await expect(win).toHaveCount(1);
    await expect(win.locator('[data-act="retry"], [data-act="discard"]')).toHaveCount(0);
    await win.locator('[data-act="close"]').click();
    await assertNoBannedWords(rp);
    await rp.screenshot({ path: path.join(SHOTS, 'tp-bu-nieprzetworzone.png'), fullPage: true });

    // The server refuses the reader the same changes the screen does not offer.
    const [record] = await serverUnprocessed(page, instanceId, 'wyniki-badan');
    const refused = await rp.evaluate(async ([iid, p, o]) => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      const out = [];
      for (const [kind, payload] of [
        ['busDlqRetryRequest', { instanceId: iid, sourceTopic: 'wyniki-badan', partition: p, offset: o }],
        ['busDlqDiscardRequest', { instanceId: iid, sourceTopic: 'wyniki-badan', partition: p, offset: o }],
        ['busDlqRetryAllRequest', { instanceId: iid, sourceTopic: 'wyniki-badan', maxRecords: 500 }],
      ]) {
        try { await ApiBinary.action(kind, payload); out.push('accepted'); } catch (e) { out.push(String(e?.message || e)); }
      }
      return out;
    }, [instanceId, record.partition, record.offset]);
    for (const r of refused) expect(r).toMatch(/permission_denied|PolicyDenied/);
    expect((await serverUnprocessed(page, instanceId, 'wyniki-badan')).length).toBe(14);
    expect(readerErrors.filter((e) => !/PolicyDenied|permission_denied|protocol error/i.test(e)), readerErrors.join('\n')).toEqual([]);
  } finally {
    await context.close();
  }
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U4 Ponów one message: the window says where it goes and who gets it; it lands at the end of the topic once, every counter follows', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wyniki-badan&section=dlq`);
  const table = unpTable(page);
  await expect(table.locator('tbody tr')).toHaveCount(10, { timeout: 20000 });
  await expectCounters(page, 17, 14);
  const endBefore = await topicEnd(page, instanceId, 'wyniki-badan');
  const retries = auditCount('bus.dlq.retry');
  const first = table.locator('tbody tr').first();
  const where = norm(await first.locator('td').nth(1).textContent());

  // Anuluj changes nothing.
  await first.locator('tf-button[data-act="retry"]').click();
  const win = unpWindow(page, 'tb-unp-confirm');
  await expect(win).toHaveCount(1);
  const impact = norm(await win.locator('[data-role="impact"]').textContent());
  expect(impact).toContain('Co się stanie po ponowieniu:');
  expect(impact).toContain('wróci do topiku wyniki-badan jako nowa, na jego koniec');
  expect(impact).toContain('Dostaną ją wszyscy odbiorcy tego topiku (aplikacja-lekarza i raporty-laboratorium) — także ci, którzy już ją przetworzyli.');
  expect(impact).toContain('po 5 próbach');
  await page.screenshot({ path: path.join(SHOTS, 'tp-nieprzetworzone-ponow.png') });
  await win.locator('[data-act="cancel"]').click();
  await expect(win).toHaveCount(0);
  expect(await topicEnd(page, instanceId, 'wyniki-badan')).toBe(endBefore);

  await first.locator('tf-button[data-act="retry"]').click();
  await win.locator('[data-act="go"]').click();
  await expect(win).toHaveCount(0, { timeout: 15000 });
  const notice = section(page, 'dlq').locator('[data-role="notice"] tf-alert');
  await expect(notice).toHaveAttribute('title', 'Ponowiono wiadomość');
  await expect(notice).toHaveAttribute('message', /wróciła do topiku wyniki-badan jako nowa/);
  await expectCounters(page, 16, 13);
  await expect(section(page, 'dlq').locator('[data-role="footer"]')).toHaveText(/Pokazano 10 z 13/);
  expect((await rowText(table)).some((r) => r.includes(where))).toBeFalsy();
  // The server: the message is at the end of its topic exactly once and left the list.
  expect(await topicEnd(page, instanceId, 'wyniki-badan')).toBe(endBefore + 1);
  expect((await serverUnprocessed(page, instanceId, 'wyniki-badan')).length).toBe(13);
  expect(auditCount('bus.dlq.retry')).toBe(retries + 1);
  await page.screenshot({ path: path.join(SHOTS, 'tp-nieprzetworzone-ponowiono.png'), fullPage: true });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U4 Odrzuć one message: said to be final, gone from the list and every counter; a message rejected at write offers no retry', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wizyty&section=dlq`);
  const table = unpTable(page);
  await expect(table.locator('tbody tr')).toHaveCount(3, { timeout: 20000 });
  const atWrite = table.locator('tbody tr', { hasText: 'przy zapisie' });
  expect(await atWrite.locator('tf-button').evaluateAll((els) => els.map((b) => b.dataset.act))).toEqual(['view', 'discard']);
  const discards = auditCount('bus.dlq.discard');

  await atWrite.locator('tf-button[data-act="discard"]').click();
  const win = unpWindow(page, 'tb-unp-confirm');
  await expect(win).toContainText('Odrzuconej wiadomości nie da się przywrócić ani ponowić.');
  await expect(win.locator('[data-role="impact"]')).toContainText('liczba zmaleje o 1');
  await expect(win.locator('[data-act="go"]')).toHaveAttribute('variant', 'danger');
  await page.screenshot({ path: path.join(SHOTS, 'tp-nieprzetworzone-odrzuc.png') });
  await win.locator('[data-act="go"]').click();
  await expect(win).toHaveCount(0, { timeout: 15000 });
  await expect(section(page, 'dlq').locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', 'Odrzucono wiadomość');
  await expect(table.locator('tbody tr')).toHaveCount(2);
  await expect(table.locator('tbody tr', { hasText: 'przy zapisie' })).toHaveCount(0);
  await expectCounters(page, 15, 2);
  expect((await serverUnprocessed(page, instanceId, 'wizyty')).length).toBe(2);
  expect(auditCount('bus.dlq.discard')).toBe(discards + 1);
  await page.screenshot({ path: path.join(SHOTS, 'tp-nieprzetworzone-odrzucono.png'), fullPage: true });
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U4 Ponów wszystkie: from a tile and from a section, what stays is counted, then nothing is left and every counter says so', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openUnprocessed(page);
  await expectCounters(page, 15);
  const visitsEnd = await topicEnd(page, instanceId, 'wizyty');

  // From the tile of wizyty: its two failed visits.
  await unpTile(page, 'wizyty').locator('[data-role="retry-all"]').click();
  const win = unpWindow(page, 'tb-unp-confirm');
  await expect(win).toContainText('Nieprzetworzone wiadomości z topiku wizyty, których odbiorca nie przetworzył (2)');
  await expect(win.locator('[data-role="impact"]')).toContainText('Na liście nie zostanie nic.');
  await expect(win.locator('[data-role="impact"]')).toContainText('Dostanie je odbiorca rejestracja-online.');
  await expect(win).toContainText('najwyżej 500 wiadomości');
  await expect(win.locator('[data-act="go"]')).toHaveText('Ponów 2 wiadomości');
  await page.screenshot({ path: path.join(SHOTS, 't06-ponow-wizyty.png') });
  await win.locator('[data-act="go"]').click();
  await expect(win).toHaveCount(0, { timeout: 30000 });
  const notice = unpSlot(page).locator('[data-role="notice"] tf-alert');
  await expect(notice).toHaveAttribute('title', 'Ponowiono 2 wiadomości');
  await expect(unpSlot(page).locator('.tb-unp-tile')).toHaveCount(1, { timeout: 15000 });
  await expectCounters(page, 13);
  await expect(unpSlot(page).locator('[data-role="footer"]')).toHaveText(/Pokazano 10 z 13/);
  expect(await topicEnd(page, instanceId, 'wizyty')).toBe(visitsEnd + 2);
  await page.screenshot({ path: path.join(SHOTS, 't06-ponowiono-wizyty.png'), fullPage: true });

  // From wyniki-badan's section: the thirteen left, then its empty state.
  const resultsEnd = await topicEnd(page, instanceId, 'wyniki-badan');
  await unpTile(page, 'wyniki-badan').click();
  const retryAll = section(page, 'dlq').locator('[data-go="unp-retry-all"]');
  await expect(retryAll).toHaveText('Ponów wszystkie (13)', { timeout: 15000 });
  await retryAll.click();
  await expect(win.locator('[data-act="go"]')).toHaveText('Ponów 13 wiadomości');
  await win.locator('[data-act="go"]').click();
  await expect(win).toHaveCount(0, { timeout: 30000 });
  await expect(section(page, 'dlq').locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', 'Ponowiono 13 wiadomości');
  await expect(section(page, 'dlq').locator('tf-empty-state')).toHaveAttribute('title', 'Wszystkie wiadomości są przetworzone', { timeout: 15000 });
  await expect(section(page, 'dlq').locator('[data-go="unp-retry-all"]')).toHaveCount(0);
  await expectCounters(page, 0, 0);
  expect(await topicEnd(page, instanceId, 'wyniki-badan')).toBe(resultsEnd + 13);
  expect(await serverUnprocessed(page, instanceId, 'wyniki-badan')).toHaveLength(0);
  // A second "Ponów wszystkie" sends nothing again.
  const again = await busCall(page, 'busDlqRetryAllRequest', { instanceId, sourceTopic: 'wyniki-badan', maxRecords: 500 });
  expect(again.retried).toBe(0);
  expect(await topicEnd(page, instanceId, 'wyniki-badan')).toBe(resultsEnd + 13);
  await page.screenshot({ path: path.join(SHOTS, 'tp-nieprzetworzone-ponowiono-wszystkie.png'), fullPage: true });

  await tab(page, 'dlq').click();
  await expect(unpSlot(page).locator('tf-empty-state')).toHaveAttribute('title', 'Wszystkie wiadomości są przetworzone', { timeout: 15000 });
  await tab(page, 'overview').click();
  await expect(overview(page).locator('tf-stat-card[data-kpi="dlq"]')).toHaveAttribute('value', '0', { timeout: 15000 });
  expect(errors, errors.join('\n')).toEqual([]);
});

// ----------------------------------------------------------------------------
// U5 — Wzory wiadomości (T08): the list, a pattern's page, and the windows
// (add, new version accepted and refused, compatibility, withdraw a version
// and the pattern, delete), each ending on the new state on screen and on
// the server.
// ----------------------------------------------------------------------------

const schemasSlot = (page) => page.locator('#tb-panel > [data-tb-view-slot="schemas"]');
const schemaSlot = (page) => page.locator('#tb-panel > [data-tb-view-slot="schema"]');
const schemaWindow = (page) => page.locator('tf-window.tb-schema-window');
const deleteWindow = (page) => page.locator('tf-window.tb-delete-window');
const schemaTable = (page) => schemasSlot(page).locator('[data-role="table"]');
const schemaRow = (page, name) => schemaTable(page).locator('tbody tr').filter({ has: page.locator('.tf-table__cell-title', { hasText: new RegExp(`^${name}$`) }) });
const versionRow = (page, n) => schemaSlot(page).locator('[data-role="versions"] tbody tr').filter({ has: page.locator('.tf-table__cell-title', { hasText: new RegExp(`^Wersja ${n}$`) }) });
const editorText = (page) => schemaSlot(page).locator('tf-code-editor').evaluate((el) => el.value);
// What the editor really draws: its visible rows in order, not its `value`.
const editorShown = (page) => schemaSlot(page).locator('tf-code-editor').evaluate((el) => [...el.shadowRoot.querySelectorAll('.row')]
  .filter((r) => r.style.display !== 'none' && r._row != null)
  .sort((a, b) => a._row - b._row)
  .map((r) => r.textContent)
  .join('\n'));
// Created by these tests under a name of this run, so a rig reused after a
// failed run does not trip over what that run left behind.
const REFERRAL = `skierowanie-${Date.now().toString(36)}`;
const subjectNames = async (page, instanceId) => (await busCall(page, 'busSchemaSubjectListRequest', { instanceId })).subjects.map((s) => s.subject).sort();

const REFERRAL_V1 = JSON.stringify({ type: 'object', required: ['pacjent', 'badanie'], properties: { pacjent: { type: 'string' }, badanie: { type: 'string' } } }, null, 2);
const REFERRAL_V2 = JSON.stringify({ type: 'object', required: ['pacjent', 'badanie', 'pilne'], properties: { pacjent: { type: 'string' }, badanie: { type: 'string' }, pilne: { type: 'boolean' } } }, null, 2);

async function openSchemas(page) {
  await openInstance(page, 'Produkcja');
  await tab(page, 'schemas').click();
  await expect(schemaTable(page).locator('tbody tr').first()).toBeVisible({ timeout: 20000 });
}

async function openSchemaPage(page, name) {
  const instance = await openInstance(page, 'Produkcja');
  await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instance}&tab=schemas&subject=${name}`);
  await expect(schemaSlot(page).locator('.tb-title')).toHaveText(name, { timeout: 20000 });
  return instance;
}

test('U5 Wzory at 1440: the list, a pattern\'s page with its text, versions and compatibility', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openSchemas(page);
  const instance = hashParams(page).instance;
  const listed = (await busCall(page, 'busSchemaSubjectListRequest', { instanceId: instance })).subjects;
  await expect(schemaTable(page).locator('tbody tr')).toHaveCount(listed.length);
  const inUse = listed.filter((s) => s.usedByTopics.length).length;
  const withdrawn = listed.filter((s) => s.deprecatedAtMs != null).length;
  await expect(schemasSlot(page).locator('[data-role="filter"] .tf-seg-opt')).toHaveText([`Wszystkie ${listed.length}`, `W użyciu ${inUse}`, `Wycofane ${withdrawn}`]);
  const used = listed.find((s) => s.subject === 'wizyta').usedByTopics.sort();
  expect(used).toContain('wizyty');
  // A used pattern's bin is a lock that says why, on a click as much as on hover.
  const blocked = schemaRow(page, 'wizyta').locator('tf-button[data-act="delete-blocked"]');
  await expect(schemaRow(page, 'wizyta').locator('tf-button[data-act="delete"]')).toHaveCount(0);
  await expect(blocked).not.toHaveAttribute('disabled', '');
  await expect(blocked).toHaveAttribute('aria-label', new RegExp(`^Nie można usunąć: używa(ją)? go topik(i)? ${used[0]}.*wybierzesz inny wzór\\.$`));
  await blocked.click();
  await expect(page.locator('.toast', { hasText: `Nie można usunąć: używa` })).toBeVisible();
  await expect(schemaSlot(page)).toBeHidden();
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 't08-wzory.png'), fullPage: true });

  await schemaRow(page, 'wizyta').locator('td').first().click();
  const p = schemaSlot(page);
  await expect(p.locator('.tb-title')).toHaveText('wizyta', { timeout: 15000 });
  expect(hashParams(page)).toMatchObject({ tab: 'schemas', subject: 'wizyta' });
  await expect(page.locator('#tb-crumbs')).toContainText('wizyta');
  await expect(p.locator('[data-role="chips"] tf-chip')).toHaveCount(3);
  await expect(p.locator('[data-role="chips"] tf-chip').nth(0)).toHaveAttribute('label', 'JSON Schema');
  await expect(p.locator('[data-role="chips"] tf-chip').nth(1)).toHaveAttribute('label', 'wersja 3');
  await expect(p.locator('[data-role="chips"] tf-chip').nth(2)).toHaveAttribute('label', 'w użyciu');
  await expect(p.locator('[data-role="desc"]')).toContainText('· Administrator ·');
  await expect(p.locator('[data-role="desc"]')).toContainText('używa');
  await expect(p.locator('[data-role="badges"] tf-chip')).toHaveAttribute('label', 'zgodność: nowe programy przeczytają stare wiadomości');
  await expect(p.locator('[data-role="text-title"]')).toHaveText('Wersja 3 — tekst wzoru');
  await expect(p.locator('[data-role="about"]')).toHaveText('Wizyta musi mieć pacjenta i termin.');
  await expect.poll(() => editorText(page), { timeout: 15000 }).toContain('"gabinet"');
  await expect.poll(() => editorShown(page), { timeout: 15000 }).toContain('"gabinet"');
  await expect(p.locator('[data-role="versions"] tbody tr')).toHaveCount(3);
  await expect(versionRow(page, 3)).toContainText('Administrator');
  await expect(versionRow(page, 3)).toContainText('aktualna');
  await expect(versionRow(page, 2)).toContainText('starsza');
  await expect(versionRow(page, 1)).toContainText('wycofana');
  await expect(versionRow(page, 1).locator('tf-button[data-act="withdraw-version"]')).toHaveCount(0);
  await expect(p.locator('[data-role="delete"]')).toHaveAttribute('disabled', '');
  await expect(p.locator('[data-role="delete-note"]')).toContainText(used[0]);
  await assertNoOverflow(page);
  await assertNoBannedWords(page);
  await page.screenshot({ path: path.join(SHOTS, 't08-wzor-wizyta.png'), fullPage: true });

  // An older version's text, then "Pobierz" and "Kopiuj" of what is shown.
  await versionRow(page, 2).locator('tf-button[data-act="show-version"]').click();
  await expect(p.locator('[data-role="text-title"]')).toHaveText('Wersja 2 — tekst wzoru');
  await expect(p.locator('[data-role="text-note"]')).toHaveText('Topiki sprawdzają wiadomości według wersji 3, nie tej.');
  await expect.poll(() => editorText(page)).not.toContain('"gabinet"');
  // The rows on screen are version 2's, not what version 3 drew before.
  await expect.poll(() => editorShown(page)).toContain('"lekarz"');
  expect(await editorShown(page)).not.toContain('"gabinet"');
  const [download] = await Promise.all([page.waitForEvent('download'), p.locator('[data-role="download"]').click()]);
  expect(download.suggestedFilename()).toBe('wizyta-v2.json');
  const saved = fs.readFileSync(await download.path(), 'utf8');
  expect(JSON.parse(saved).properties.lekarz).toBeTruthy();
  expect(saved).not.toContain('gabinet');
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'], { origin: `https://127.0.0.1:${PORT}` });
  await p.locator('[data-role="copy"]').click();
  await expect(page.locator('.toast', { hasText: 'Skopiowano tekst wersji 2' })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(saved);

  // A reload opens the same pattern.
  await page.reload();
  await expect(schemaSlot(page).locator('.tb-title')).toHaveText('wizyta', { timeout: 20000 });
  await expect(tab(page, 'schemas')).toHaveAttribute('aria-selected', 'true');
  await schemaSlot(page).locator('[data-go="back"]').first().click();
  await expect(schemaTable(page).locator('tbody tr')).toHaveCount(listed.length);
  expect(hashParams(page).subject).toBeUndefined();
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U5 Dodaj wzór, then a new version refused in plain words, the compatibility changed, the version added', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  await openSchemas(page);
  const instance = hashParams(page).instance;
  const before = (await subjectNames(page, instance)).length;
  await schemasSlot(page).locator('[data-go="add"]').first().click();
  const win = schemaWindow(page);
  await expect(win.locator('[slot="body"]')).toBeVisible();
  await expect(win.locator('tf-choice-card')).toHaveAttribute('heading', 'JSON Schema');
  const save = win.locator('[data-act="save"]');
  await win.locator('[data-role="name"] input').fill('wizyta');
  await expect(win.locator('[data-role="name"]')).toHaveAttribute('error', /już jest/);
  await expect(win.locator('[data-role="impact"]')).toContainText('Popraw zaznaczone pole');
  await win.locator('[data-role="name"] input').fill(REFERRAL);
  await win.locator('[data-role="text"] textarea').fill('{"type": ');
  await expect(win.locator('[data-role="text"]')).toHaveAttribute('error', /To nie jest poprawny JSON/);
  await expect(save).toHaveAttribute('disabled', '');
  await win.locator('[data-role="text"] textarea').fill(REFERRAL_V1);
  await expect(win.locator('[data-role="impact"]')).toHaveText(`Co się stanie po dodaniu: powstanie wzór ${REFERRAL} (JSON Schema), wersja 1. Żaden topik go jeszcze nie używa — wybierzesz go w ustawieniach topiku.`);
  await page.screenshot({ path: path.join(SHOTS, 't08-dodaj.png') });
  await save.click();
  await expect(win).toHaveCount(0);
  await expect(schemasSlot(page).locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', `Dodano wzór ${REFERRAL}`);
  await expect(schemaTable(page).locator('tbody tr')).toHaveCount(before + 1, { timeout: 15000 });
  await expect(page.locator('#tb-tabs tf-tab#schemas')).toHaveAttribute('count', String(before + 1));
  await expect(schemaRow(page, REFERRAL)).toContainText('nieużywany');
  await page.screenshot({ path: path.join(SHOTS, 't08-dodano.png'), fullPage: true });

  await schemaRow(page, REFERRAL).locator('td').first().click();
  const p = schemaSlot(page);
  await expect(p.locator('.tb-title')).toHaveText(REFERRAL, { timeout: 15000 });
  await expect(p.locator('[data-role="delete"]')).not.toHaveAttribute('disabled', '');

  // A new required field breaks "nowe programy przeczytają stare wiadomości".
  await p.locator('[data-role="new-version"]').click();
  await expect(win.locator('[data-role="text"] textarea')).toHaveValue(REFERRAL_V1);
  await win.locator('[data-role="text"] textarea').fill(REFERRAL_V2);
  await expect(win.locator('[data-role="diff"]')).toHaveText('Różnica względem wersji 1: nowe, wymagane pole „pilne”.');
  await win.locator('[data-act="save"]').click();
  const refusal = win.locator('[data-role="error"]');
  await expect(refusal).toBeVisible({ timeout: 15000 });
  await expect(refusal).toHaveText('Nie dodano wersji 2. Ten wzór ma zgodność „nowe programy przeczytają stare wiadomości”, a nowa wersja wymaga pola „pilne”, którego stare wiadomości mogą nie mieć. Usuń „pilne” z pól wymaganych albo zmień zgodność wzoru.');
  await expect(win).toHaveCount(1);
  await expect(win.locator('[data-act="save"]')).toHaveAttribute('disabled', '');
  await page.screenshot({ path: path.join(SHOTS, 't08-wzor-nowa-wersja-odmowa.png') });
  await win.locator('[data-act="cancel"]').click();
  await expect(win).toHaveCount(0);
  expect((await busCall(page, 'busSchemaVersionListRequest', { instanceId: instance, subject: REFERRAL })).versions).toHaveLength(1);

  await p.locator('[data-role="compat"]').click();
  await win.locator('tf-radio[value="none"]').click();
  await expect(win.locator('[data-role="impact"]')).toContainText(`każda kolejna wersja wzoru ${REFERRAL} będzie sprawdzana warunkiem „bez sprawdzania”`);
  await win.locator('[data-act="save"]').click();
  await expect(win).toHaveCount(0);
  await expect(p.locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', 'Zapisano zgodność');
  await expect(p.locator('[data-role="badges"] tf-chip')).toHaveAttribute('label', 'zgodność: bez sprawdzania', { timeout: 15000 });

  // "Nowa wersja" comes back with the refused text.
  await p.locator('[data-role="new-version"]').click();
  await expect(win.locator('.tb-explain-box')).toContainText('z poprzedniej próby');
  await expect(win.locator('[data-role="text"] textarea')).toHaveValue(REFERRAL_V2);
  await win.locator('[data-act="save"]').click();
  await expect(win).toHaveCount(0, { timeout: 15000 });
  await expect(p.locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', 'Dodano wersję 2');
  await expect(p.locator('[data-role="chips"] tf-chip').nth(1)).toHaveAttribute('label', 'wersja 2', { timeout: 15000 });
  await expect(p.locator('[data-role="versions"] tbody tr')).toHaveCount(2);
  await expect(versionRow(page, 2)).toContainText('aktualna');
  await expect.poll(() => editorText(page)).toContain('"pilne"');
  await expect.poll(() => editorShown(page)).toContain('"pilne"');
  expect((await busCall(page, 'busSchemaVersionListRequest', { instanceId: instance, subject: REFERRAL })).versions.map((v) => v.version)).toEqual([1, 2]);
  expect(errors.filter((e) => !/schema_incompatible|BadRequest/.test(e)), errors.join('\n')).toEqual([]);
});

test('U5 withdraw a version, withdraw the pattern, delete it; a pattern a topic uses cannot be deleted', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instance = await openSchemaPage(page, REFERRAL);
  const p = schemaSlot(page);
  const confirm = page.locator('tf-window.tb-schema-window');

  await versionRow(page, 2).locator('tf-button[data-act="withdraw-version"]').click();
  await expect(confirm.locator('[data-role="impact"]')).toHaveText('Co się stanie po wycofaniu: najnowszą niewycofaną wersją stanie się wersja 1.');
  await expect(confirm.locator('.tb-foot-note')).toContainText('dzienniku audytu');
  await page.screenshot({ path: path.join(SHOTS, 't08-wzor-wycofaj-wersje.png') });
  await confirm.locator('[data-act="go"]').click();
  await expect(confirm).toHaveCount(0);
  await expect(p.locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', 'Wycofano wersję 2');
  await expect(versionRow(page, 2)).toContainText('wycofana', { timeout: 15000 });
  await expect(versionRow(page, 1)).toContainText('aktualna');
  const afterVersion = (await busCall(page, 'busSchemaVersionListRequest', { instanceId: instance, subject: REFERRAL })).versions;
  expect(afterVersion.find((v) => v.version === 2).deprecatedAtMs).toBeTruthy();
  expect(afterVersion.find((v) => v.version === 1).deprecatedAtMs ?? null).toBeNull();

  await p.locator('[data-role="withdraw"]').click();
  await expect(confirm.locator('.tb-explain-box')).toContainText(`Wzór ${REFERRAL} i 1 jego niewycofana wersja zostaną oznaczone jako wycofane.`);
  await confirm.locator('[data-act="go"]').click();
  await expect(confirm).toHaveCount(0);
  await expect(p.locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', `Wycofano wzór ${REFERRAL}`);
  // The note of the withdrawal stands alone; the warning is what a later visit shows.
  await expect(p.locator('[data-role="warning"] tf-alert')).toHaveCount(0);
  await expect(p.locator('[data-role="new-version"]')).toHaveAttribute('disabled', '');
  await expect(p.locator('[data-role="withdraw"]')).toHaveAttribute('disabled', '');
  await expect(p.locator('[data-role="compat"]')).toHaveCount(0);
  await expect(versionRow(page, 1)).toContainText('wycofana');
  await page.screenshot({ path: path.join(SHOTS, 't08-wzor-wycofano.png'), fullPage: true });
  const subjects = (await busCall(page, 'busSchemaSubjectListRequest', { instanceId: instance })).subjects;
  expect(subjects.find((s) => s.subject === REFERRAL).deprecatedAtMs).toBeTruthy();

  await p.locator('[data-role="delete"]').click();
  const del = deleteWindow(page);
  await expect(del.locator('.tb-danger-box')).toContainText(`Wzór ${REFERRAL} (JSON Schema) i wszystkie jego wersje znikną.`);
  await expect(del.locator('[data-action="confirm"]')).toHaveAttribute('disabled', '');
  await del.locator('#retype-input input').fill(REFERRAL);
  await del.locator('[data-action="confirm"]').click();
  await expect(del).toHaveCount(0, { timeout: 15000 });
  await expect(schemasSlot(page).locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', `Usunięto wzór ${REFERRAL}`);
  await expect(schemaRow(page, REFERRAL)).toHaveCount(0, { timeout: 15000 });
  expect(await subjectNames(page, instance)).not.toContain(REFERRAL);
  expect(hashParams(page).subject).toBeUndefined();

  // The withdrawn, unused seeded pattern goes from its row (a rig reused
  // after this already ran has none left to delete).
  if ((await subjectNames(page, instance)).includes('wizyta-2025')) {
    await schemasSlot(page).locator('[data-role="filter"] .tf-seg-opt', { hasText: 'Wycofane' }).click();
    await schemaRow(page, 'wizyta-2025').locator('tf-button[data-act="delete"]').click();
    await expect(del.locator('.tb-danger-box')).toContainText('Wzór wizyta-2025 (JSON Schema) i wszystkie jego wersje znikną.');
    await del.locator('#retype-input input').fill('wizyta-2025');
    await del.locator('[data-action="confirm"]').click();
    await expect(del).toHaveCount(0, { timeout: 15000 });
    await expect(schemasSlot(page).locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', 'Usunięto wzór wizyta-2025');
    await expect(schemaRow(page, 'wizyta-2025')).toHaveCount(0, { timeout: 15000 });
    await schemasSlot(page).locator('[data-role="filter"] .tf-seg-opt', { hasText: 'Wszystkie' }).click();
  }

  // A pattern a topic uses: no delete on screen, and the server refuses it too.
  await expect(schemaRow(page, 'wizyta').locator('tf-button[data-act="delete"]')).toHaveCount(0);
  await expect(schemaRow(page, 'wizyta').locator('tf-button[data-act="delete-blocked"]')).toHaveCount(1);
  const refused = await page.evaluate(async (iid) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    try {
      await ApiBinary.action('busSchemaDeleteRequest', { instanceId: iid, subject: 'wizyta', deprecateOnly: false });
      return null;
    } catch (err) {
      return String(err?.message || err);
    }
  }, instance);
  expect(refused).toMatch(/is bound by topics: .*wizyty/);
  expect(await subjectNames(page, instance)).toContain('wizyta');
  expect(errors.filter((e) => !/bound by topics|BadRequest/.test(e)), errors.join('\n')).toEqual([]);
});

test('U5 at 390x844: the list as cards, the pattern\'s page in one column, a window fills the phone', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(PHONE);
  await login(page);
  await openSchemas(page);
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 't08-wzory-u5-telefon.png'), fullPage: true });
  await schemasSlot(page).locator('[data-go="add"]').first().click();
  await expect(schemaWindow(page).locator('[slot="body"]')).toBeVisible();
  await windowFits(page, 'tf-window.tb-schema-window', PHONE.width);
  await page.screenshot({ path: path.join(SHOTS, 't08-dodaj-telefon.png') });
  await schemaWindow(page).locator('[data-act="cancel"]').click();
  await expect(schemaWindow(page)).toHaveCount(0);
  await schemaRow(page, 'wizyta').locator('td').first().click();
  const p = schemaSlot(page);
  await expect(p.locator('.tb-title')).toHaveText('wizyta', { timeout: 15000 });
  await expect.poll(() => editorText(page), { timeout: 15000 }).toContain('"gabinet"');
  const cols = await p.locator('.tb-schema-grid').evaluate((el) => getComputedStyle(el).gridTemplateColumns.split(' ').length);
  expect(cols).toBe(1);
  await assertNoOverflow(page);
  await page.screenshot({ path: path.join(SHOTS, 't08-wzor-wizyta-telefon.png'), fullPage: true });
  await p.locator('[data-role="compat"]').click();
  await expect(schemaWindow(page).locator('[slot="body"]')).toBeVisible();
  await windowFits(page, 'tf-window.tb-schema-window', PHONE.width);
  await schemaWindow(page).locator('[data-act="cancel"]').click();
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U5 without administration: the patterns to read, no change buttons, who changes them', async ({ page, browser }) => {
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  const context = await browser.newContext({ ignoreHTTPSErrors: true, locale: 'pl-PL', viewport: DESKTOP });
  const rp = await context.newPage();
  const readerErrors = trackErrors(rp);
  try {
    await rp.addInitScript(() => {
      localStorage.setItem('tentaflow_lang', 'pl');
      document.addEventListener('DOMContentLoaded', () => {
        const st = document.createElement('style');
        st.textContent = '.update-overlay{display:none!important}';
        document.head.appendChild(st);
      });
    });
    await loginAsAdmin(rp, { port: PORT, username: 'tomasz', password: 'Tomasz-czyta-1' });
    await rp.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=schemas`);
    await expect(schemaRow(rp, 'wizyta')).toHaveCount(1, { timeout: 20000 });
    await expect(schemasSlot(rp).locator('[data-go="add"]')).toHaveCount(0);
    await expect(schemasSlot(rp).locator('.tb-admin-note')).toContainText('administrator instancji');
    await expect(schemaRow(rp, 'wizyta').locator('tf-button[data-act="delete"], tf-button[data-act="delete-blocked"]')).toHaveCount(0);
    await schemaRow(rp, 'wizyta').locator('td').first().click();
    const p = schemaSlot(rp);
    await expect(p.locator('.tb-title')).toHaveText('wizyta', { timeout: 15000 });
    await expect(p.locator('[data-role="actions"] tf-button')).toHaveCount(0);
    await expect(p.locator('[data-role="actions"]')).toContainText('Wzory dodaje, zmienia, wycofuje i usuwa administrator instancji.');
    await expect(p.locator('[data-role="compat"]')).toHaveCount(0);
    await expect(p.locator('[data-role="versions"] tf-button[data-act="withdraw-version"]')).toHaveCount(0);
    await expect.poll(() => p.locator('tf-code-editor').evaluate((el) => el.value), { timeout: 15000 }).toContain('"gabinet"');
    await rp.screenshot({ path: path.join(SHOTS, 't08-wzor-bez-uprawnien.png'), fullPage: true });
    expect(readerErrors.filter((e) => !/PolicyDenied|permission_denied|protocol error/i.test(e)), readerErrors.join('\n')).toEqual([]);
  } finally {
    await context.close();
  }
});

// ----------------------------------------------------------------------------
// U6 — a topic's Dostęp (T09): the entries of people, groups and addons, and
// the keys of outside systems. What the screen writes is checked against the
// server's own list of entries and, for a key, against the records REST the
// key is for.
// ----------------------------------------------------------------------------

const accessSection = (page) => section(page, 'access');
// Names made in these tests carry the run, so a rerun against the same node
// never meets the previous run's group or key.
const RUN = Date.now().toString(36).slice(-5);
const accessWindow = (page) => page.locator('tf-window.tb-access-window');
const subjectRow = (page, name) => accessSection(page).locator('[data-role="subjects"] tbody tr', { hasText: name });

function sqliteWrite(sql) {
  execFileSync('/usr/bin/sqlite3', ['-cmd', '.timeout 5000', DB, sql], { encoding: 'utf8' });
}

function iamCall(page, kind, payload) {
  return page.evaluate(async ([k, p]) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return k === 'iamListUsersRequest' ? ApiBinary.one(k, p) : ApiBinary.action(k, p);
  }, [kind, payload]);
}

async function ensureUser(page, username, password, displayName) {
  const users = (await iamCall(page, 'iamListUsersRequest', {}))?.users || [];
  const found = users.find((u) => u.username === username);
  if (found) return found.userId ?? found.user_id ?? found.id;
  const created = await iamCall(page, 'iamCreateUserRequest', { username, password, displayName, email: '', role: 'user', groupIds: [] });
  return created?.userId ?? created?.user_id;
}

async function serverEntries(page, instanceId, topic = 'wyniki-badan') {
  const res = await busCall(page, 'busAclListRequest', { instanceId, topic });
  return (res?.entries || []).map((e) => `${e.subjectType}:${e.subjectId}:${e.action}:${e.accessLevel}`).sort();
}

// A window's entry and exit are animated: the evidence is taken once they
// end (a status dot pulses forever and is not waited for).
async function settled(page) {
  await page.waitForFunction(() => document.getAnimations({ subtree: true })
    .every((a) => a.playState !== 'running' || a.effect?.getTiming().iterations === Infinity));
}

async function pickSegment(win, selector, label) {
  await win.locator(`${selector} .tf-seg-opt`, { hasText: label }).click();
}

test('U6 Dostęp at 1440: a group gets reading, a person a write ban, a change, the ban removed with its warning, an addon', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  const tomaszId = await ensureUser(page, 'tomasz', 'Tomasz-czyta-1', 'Tomasz Nowak');
  const piotrId = await ensureUser(page, 'piotr', 'Piotr-zakaz-1', 'Piotr Zieliński');
  const groupName = `Rejestracja ${RUN}`;
  const groupId = (await iamCall(page, 'iamCreateGroupRequest', { name: groupName, description: '' }))?.groupId;
  expect(groupId).toBeTruthy();
  await iamCall(page, 'iamSetUserGroupsRequest', { userId: tomaszId, groupIds: [groupId] });
  // An addon that declares the bus and holds bus.read in this instance: the
  // directory offers exactly those.
  sqliteWrite("INSERT OR IGNORE INTO addons (addon_id, name, version, display_name) VALUES ('e2e-asystent', 'e2e-asystent', '1.0.0', 'Asystent lekarza');"
    + " INSERT OR IGNORE INTO addon_permission_catalog (addon_id, permission_id) VALUES ('e2e-asystent', 'bus.subscribe');");
  await iamCall(page, 'addonPermissionSetRequest', { addonId: instanceId, subjectType: 'user', subjectId: 'e2e-asystent', permissionId: 'bus.read', grantMode: 'allow' });
  const before = await serverEntries(page, instanceId);
  try {
    await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wyniki-badan&section=access`);
    const d = detailSlot(page);
    await expect(d.locator('.tb-title')).toHaveText('wyniki-badan', { timeout: 20000 });
    await expect(d.locator('[data-role="menu"]')).toHaveAttribute('value', 'access');
    const s = accessSection(page);
    await expect(s.locator('.section-card').first()).toContainText('Osoby, grupy i addony');
    await expect(s.locator('[data-role="subjects-count"] tf-chip')).toBeVisible({ timeout: 15000 });

    // Nadaj: the group Rejestracja reads.
    await s.locator('tf-button[data-go="access-grant"]').click();
    const win = accessWindow(page);
    await expect(win).toHaveCount(1);
    await expect(win.locator('[data-role="subject"] select option', { hasText: `${groupName} (1 osoba)` })).toHaveCount(1, { timeout: 15000 });
    await win.locator('[data-role="subject"] select').selectOption({ label: `${groupName} (1 osoba)` });
    await expect(win.locator('[data-role="impact"]')).toContainText(`1 osoba z grupy ${groupName} będzie mogła czytać wiadomości w topiku wyniki-badan. O zapisie i administracji zdecyduje rola w organizacji.`);
    await windowFits(page, 'tf-window.tb-access-window', DESKTOP.width);
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-dostep-nadaj.png') });
    await win.locator('[data-act="save"]').click();
    await expect(win).toHaveCount(0);
    await expect(s.locator('tf-alert')).toHaveAttribute('title', 'Nadano dostęp');
    await expect(subjectRow(page, 'Rejestracja')).toContainText('Grupa · 1 osoba');
    await expect(subjectRow(page, 'Rejestracja')).toContainText('pozwolono');
    expect(await serverEntries(page, instanceId)).toContain(`group:${groupId}:read:allow`);

    // Nadaj: Piotr may not write; reading is left to his role.
    await s.locator('tf-button[data-go="access-grant"]').click();
    await pickSegment(win, '[data-role="kind"]', 'Użytkownik');
    await expect(win.locator('[data-role="subject"]')).toHaveAttribute('label', 'Który użytkownik');
    await expect(win.locator('[data-role="subject"] select option', { hasText: 'Piotr Zieliński' })).toHaveCount(1, { timeout: 15000 });
    await win.locator('[data-role="subject"] select').selectOption({ label: 'Piotr Zieliński' });
    await pickSegment(win, 'tf-segmented[data-right="read"]', 'Nie ustawiaj');
    await pickSegment(win, 'tf-segmented[data-right="write"]', 'Zabroń');
    await expect(win.locator('[data-role="impact"]')).toContainText('Zakaz dla „Piotr Zieliński”: zapis — wygrywa z rolą w organizacji i z pozwoleniem z grupy.');
    await win.locator('[data-act="save"]').click();
    await expect(win).toHaveCount(0);
    await expect(subjectRow(page, 'Piotr Zieliński')).toContainText('zabroniono');
    let server = await serverEntries(page, instanceId);
    expect(server).toContain(`user:${piotrId}:write:deny`);
    expect(server.filter((e) => e.startsWith(`user:${piotrId}:`))).toHaveLength(1);
    await page.screenshot({ path: path.join(SHOTS, 'tp-dostep.png'), fullPage: true });

    // Zmień: Rejestracja may also write — one change, said before saving.
    await subjectRow(page, groupName).locator('tf-button[data-act="change"]').click();
    await expect(win.locator('.tb-explain-box')).toContainText(groupName);
    await expect(win.locator('[data-act="save"]')).toHaveAttribute('disabled', '');
    await pickSegment(win, 'tf-segmented[data-right="write"]', 'Pozwól');
    await expect(win.locator('[data-role="impact"]')).toContainText(`Zapis dla „${groupName}” zmieni się z „nie ustawiono” na „pozwolono”. Będzie można wysyłać wiadomości. Pozostałe prawa bez zmian.`);
    await win.locator('[data-act="save"]').click();
    await expect(win).toHaveCount(0);
    await expect(s.locator('tf-alert')).toHaveAttribute('title', 'Zapisano');
    expect(await serverEntries(page, instanceId)).toContain(`group:${groupId}:write:allow`);

    // Usuń the ban: the window warns that it may give the right back.
    await subjectRow(page, 'Piotr Zieliński').locator('tf-button[data-act="remove"]').click();
    await expect(win.locator('[data-role="lifts-deny"]')).toHaveText(/Usunięcie „zabroniono” może dać te prawa, jeśli pozwala na nie rola w organizacji\./);
    await expect(win).toContainText('Piotr Zieliński straci wpis w tym topiku: zapis (zabroniono).');
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-dostep-usun.png') });
    await win.locator('[data-act="go"]').click();
    await expect(win).toHaveCount(0);
    await expect(subjectRow(page, 'Piotr Zieliński')).toHaveCount(0);
    await expect(s.locator('tf-alert')).toHaveAttribute('title', 'Usunięto wpis');
    server = await serverEntries(page, instanceId);
    expect(server.filter((e) => e.startsWith(`user:${piotrId}:`))).toEqual([]);

    // Nadaj to an addon: its own kind of entry, never a user with its id.
    await s.locator('tf-button[data-go="access-grant"]').click();
    await pickSegment(win, '[data-role="kind"]', 'Addon');
    await expect(win.locator('[data-role="subject"] select option', { hasText: 'Asystent lekarza' })).toHaveCount(1, { timeout: 15000 });
    await win.locator('[data-role="subject"] select').selectOption({ label: 'Asystent lekarza' });
    await expect(win.locator('[data-role="impact"]')).toContainText('Addon „Asystent lekarza” dostanie w topiku wyniki-badan: czytanie.');
    await win.locator('[data-act="save"]').click();
    await expect(win).toHaveCount(0);
    await expect(subjectRow(page, 'Asystent lekarza')).toContainText('Addon');
    expect(await serverEntries(page, instanceId)).toContain('addon:e2e-asystent:read:allow');

    const rows = await s.locator('[data-role="subjects"] tbody tr').count();
    await expect(d.locator('[data-role="menu"] tf-tab#access')).toHaveAttribute('count', String(rows));
    await assertNoBannedWords(page);
    await assertNoOverflow(page);
  } finally {
    for (const [subjectType, subjectId] of [['group', groupId], ['addon', 'e2e-asystent'], ['user', piotrId]]) {
      for (const action of ['read', 'write', 'admin']) {
        await busCall(page, 'busAclSetRequest', { instanceId, topic: 'wyniki-badan', subjectType, subjectId, accessLevel: 'clear', action });
      }
    }
    await iamCall(page, 'iamDeleteGroupRequest', { groupId });
  }
  expect(await serverEntries(page, instanceId)).toEqual(before);
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U6 keys: issued with reading, shown once; the key reads over REST and cannot write; its rights change; revoked, its rights and its reading go with it', async ({ page, request }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  const keyName = `Portal wyników ${RUN}`;
  let keyId = null;
  let revoked = false;
  try {
    await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wyniki-badan&section=access`);
    const s = accessSection(page);
    await expect(s.locator('[data-role="keys-sub"]')).toContainText('Klucz działa w instancji Produkcja', { timeout: 20000 });
    await s.locator('tf-button[data-go="key-issue"]').click();
    const win = accessWindow(page);
    await expect(win.locator('[data-role="impact"]')).toContainText('Wpisz nazwę systemu.');
    await win.locator('[data-role="name"] input').fill(keyName);
    await expect(win.locator('[data-role="impact"]')).toContainText('Zaznacz co najmniej jedno prawo.');
    await win.locator('tf-checkbox[data-key-right="readMessages"] .tf-checkbox-label').click();
    await expect(win.locator('[data-role="impact"]')).toContainText(`${keyName} dostanie klucz z prawami: czyta wiadomości (topik wyniki-badan). Klucz zobaczysz tylko raz.`);
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-dostep-wydaj-klucz.png') });
    await win.locator('[data-act="save"]').click();

    const issued = page.locator('tf-window.tb-key-issued');
    await expect(issued).toHaveCount(1);
    const token = (await issued.locator('[data-role="token"]').textContent()).trim();
    expect(token).toMatch(/^sk-[0-9a-f]{64}$/);
    const url = (await issued.locator('[data-role="url"]').textContent()).trim();
    expect(url).toBe(`https://127.0.0.1:${PORT}/v1/bus/instances/${instanceId}/topics/wyniki-badan/records?org_id=org-default`);
    const group = (await issued.locator('[data-role="group"]').textContent()).trim();
    expect(group).toMatch(/^k:[0-9a-f-]{36}$/);
    keyId = group.slice(2);
    await expect(issued).toContainText('Produkcja ·');
    await expect(issued).toContainText('czyta wiadomości · topik wyniki-badan');
    // The technical hint is folded away; Escape before copying asks first.
    await expect(issued.locator('details[data-role="developer"]')).not.toHaveAttribute('open', '');
    await page.keyboard.press('Escape');
    await expect(issued.locator('[data-role="discard"]')).toBeVisible();
    await expect(issued).toHaveCount(1);
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-dostep-klucz-wydany.png') });
    await issued.locator('[data-act="done"]').click();
    await expect(issued).toHaveCount(0);
    await expect(s.locator('tf-alert')).toHaveAttribute('title', 'Wydano klucz');
    const keyRow = s.locator('[data-role="keys"] tbody tr', { hasText: keyName });
    await expect(keyRow).toContainText('Czyta wiadomości');
    await expect(keyRow).toContainText('jeszcze nie');
    await expect(keyRow).not.toContainText('Wysyła wiadomości');

    // The key reads the topic under its own group, and nothing more.
    const auth = { Authorization: `Bearer ${token}` };
    const read = await request.get(`${url}&group=${encodeURIComponent(group)}&max_records=5&wait_ms=0`, { headers: auth });
    expect(read.status(), await read.text()).toBe(200);
    expect((await read.json()).records.length).toBeGreaterThan(0);
    const line = JSON.stringify({ payload_b64: Buffer.from('MSH|^~\\&|LAB|X|||20260929||ORU^R01|1|P|2.5').toString('base64') });
    const write = await request.post(url, { headers: { ...auth, 'Content-Type': 'application/x-ndjson' }, data: `${line}\n` });
    expect(write.status(), await write.text()).toBe(403);

    // Prawa klucza: add sending; the key stays the same.
    await keyRow.locator('tf-button[data-act="key-rights"]').click();
    await win.locator('tf-checkbox[data-key-right="writeMessages"] .tf-checkbox-label').click();
    await expect(win.locator('[data-role="impact"]')).toContainText(`${keyName} dostanie prawa: wysyła wiadomości. Pozostałe prawa bez zmian; klucz się nie zmienia.`);
    await win.locator('[data-act="save"]').click();
    await expect(win).toHaveCount(0);
    await expect(keyRow).toContainText('Wysyła wiadomości');
    const keyEntries = async () => (await busCall(page, 'busAclListRequest', { instanceId, topic: 'wyniki-badan' })).entries
      .filter((e) => e.subjectType === 'api_key' && e.subjectId === keyId)
      .map((e) => `${e.action}:${e.accessLevel}`).sort();
    expect(await keyEntries()).toEqual(['read:allow', 'write:allow']);

    // Its consumer is named after the key on the topic's Stan.
    const d = detailSlot(page);
    await d.locator('[data-role="menu"] tf-tab#state > button').click();
    const consumerRow = section(page, 'state').locator('.tb-consumer-row', { hasText: `Klucz ${keyName}` });
    await expect(consumerRow).toHaveCount(1, { timeout: 20000 });
    await expect(section(page, 'state').locator('.tb-consumer-row', { hasText: group })).toHaveCount(0);
    await d.locator('[data-role="menu"] tf-tab#access > button').click();

    // Unieważnij: at once, for good, its rights with it.
    await keyRow.locator('tf-button[data-act="key-revoke"]').click();
    await expect(win.locator('[data-role="impact"]')).toContainText(`Klucz przestanie działać od razu. ${keyName} straci prawa: czyta wiadomości i wysyła wiadomości.`);
    await win.locator('[data-act="go"]').click();
    await expect(win).toHaveCount(0);
    revoked = true;
    await expect(keyRow).toHaveCount(0);
    await expect(s.locator('tf-alert')).toHaveAttribute('title', 'Unieważniono klucz');
    expect(await keyEntries()).toEqual([]);
    await expect(s.locator('[data-role="keys"] tbody tr', { hasText: 'Klucz usunięty' })).toHaveCount(0);
    const refused = await request.get(`${url}&group=${encodeURIComponent(group)}&max_records=5&wait_ms=0`, { headers: auth });
    expect(refused.status()).toBe(401);

    // Its consumer stays on Stan, said to be gone and counted nowhere.
    await d.locator('[data-role="menu"] tf-tab#state > button').click();
    const goneRow = section(page, 'state').locator('.tb-consumer-row', { hasText: 'Klucz usunięty' });
    await expect(goneRow).toHaveCount(1, { timeout: 20000 });
    await expect(goneRow).toContainText('klucza już nie ma — nikt tu nie czyta');
  } finally {
    if (keyId && !revoked) await page.evaluate(async (id) => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      await ApiBinary.action('apiKeyRevokeRequest', { keyId: id }).catch(() => {});
    }, keyId);
  }
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U6 at 390x844: Dostęp from the section list, the rows as cards, a window fills the phone', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(PHONE);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  await busCall(page, 'busAclSetRequest', { instanceId, topic: 'wyniki-badan', subjectType: 'user', subjectId: 'e2e-telefon', accessLevel: 'deny', action: 'read' });
  try {
    await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wyniki-badan`);
    const d = detailSlot(page);
    await expect(d.locator('.tb-title')).toHaveText('wyniki-badan', { timeout: 20000 });
    await d.locator('[data-role="pick"] select').selectOption('access');
    await expect.poll(() => hashParams(page).section).toBe('access');
    const s = accessSection(page);
    await expect(s.locator('[data-role="subjects"] tbody tr').first()).toBeVisible({ timeout: 15000 });
    await assertNoOverflow(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-dostep-390.png'), fullPage: true });
    await s.locator('tf-button[data-go="access-grant"]').click();
    await expect(accessWindow(page).locator('[data-role="subject"] select option').first()).toBeAttached({ timeout: 15000 });
    await windowFits(page, 'tf-window.tb-access-window', PHONE.width);
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-dostep-nadaj-390.png') });
    await accessWindow(page).locator('[data-act="cancel"]').click();
    await expect(accessWindow(page)).toHaveCount(0);
  } finally {
    await busCall(page, 'busAclSetRequest', { instanceId, topic: 'wyniki-badan', subjectType: 'user', subjectId: 'e2e-telefon', accessLevel: 'clear', action: 'read' });
  }
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U6 without administration: no Dostęp in the menu or the section list, and an address naming it opens Stan', async ({ page, browser }) => {
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  await ensureUser(page, 'tomasz', 'Tomasz-czyta-1', 'Tomasz Nowak');
  const context = await browser.newContext({ ignoreHTTPSErrors: true, locale: 'pl-PL', viewport: DESKTOP });
  const rp = await context.newPage();
  const readerErrors = trackErrors(rp);
  try {
    await rp.addInitScript(() => {
      localStorage.setItem('tentaflow_lang', 'pl');
      document.addEventListener('DOMContentLoaded', () => {
        const st = document.createElement('style');
        st.textContent = '.update-overlay{display:none!important}';
        document.head.appendChild(st);
      });
    });
    await loginAsAdmin(rp, { port: PORT, username: 'tomasz', password: 'Tomasz-czyta-1' });
    await rp.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wyniki-badan&section=access`);
    const d = rp.locator('#tb-panel > [data-tb-view-slot="detail"]');
    await expect(d.locator('.tb-title')).toHaveText('wyniki-badan', { timeout: 20000 });
    await expect(d.locator('[data-role="menu"]')).toHaveAttribute('value', 'state');
    await expect(d.locator('[data-role="menu"] tf-tab#access')).toBeHidden();
    await expect(d.locator('[data-role="menu"] tf-tab#settings')).toBeVisible();
    await expect(d.locator('[data-section="access"]')).toBeHidden();
    const options = await d.locator('[data-role="pick"] select option').allTextContents();
    expect(options).not.toContain('Dostęp');
    expect(readerErrors.filter((e) => !/PolicyDenied|permission_denied|protocol error/i.test(e)), readerErrors.join('\n')).toEqual([]);
  } finally {
    await context.close();
  }
});

// ----------------------------------------------------------------------------
// U7 — a topic's Ukrywanie danych (T07): the rules of what each person, group
// or addon sees or may write. What the screen writes is checked against the
// server's own list of rules, and the preview against the record the server
// really projects (and the audit entry it leaves).
// ----------------------------------------------------------------------------

const hidingSection = (page) => section(page, 'hiding');
const hidingWindow = (page) => page.locator('tf-window.tb-hiding-window');
const previewWindow = (page) => page.locator('tf-window.tb-hiding-preview');
const ruleRow = (page, name) => hidingSection(page).locator('[data-role="rules"] tbody tr', { hasText: name });
const fieldSeg = (win, field) => win.locator(`tf-segmented[data-field="${field}"]`);

async function serverRules(page, instanceId, topic) {
  const res = await busCall(page, 'busFieldPolicyListRequest', { instanceId, topic });
  return Object.fromEntries((res?.policies || []).map((p) => [`${p.subjectType}:${p.subjectId}:${p.direction}`, { fields: [...p.fields].sort(), required: [...p.requiredFields].sort() }]));
}

async function clearRules(page, instanceId, topic) {
  const res = await busCall(page, 'busFieldPolicyListRequest', { instanceId, topic });
  for (const p of res?.policies || []) {
    await busCall(page, 'busFieldPolicyDeleteRequest', { instanceId, topic, subjectType: p.subjectType, subjectId: p.subjectId, direction: p.direction });
  }
}

test('U7 Ukrywanie danych at 1440: a group rule added, changed, previewed as the group; one for everyone narrows the preview; a writing rule; all deleted', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  const tomaszId = await ensureUser(page, 'tomasz', 'Tomasz-czyta-1', 'Tomasz Nowak');
  const groupName = `Rejestracja U7 ${RUN}`;
  const groupId = (await iamCall(page, 'iamCreateGroupRequest', { name: groupName, description: '' }))?.groupId;
  expect(groupId).toBeTruthy();
  await iamCall(page, 'iamSetUserGroupsRequest', { userId: tomaszId, groupIds: [groupId] });
  await clearRules(page, instanceId, 'wizyty');
  try {
    await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wizyty&section=hiding`);
    const d = detailSlot(page);
    await expect(d.locator('.tb-title')).toHaveText('wizyty', { timeout: 20000 });
    await expect(d.locator('[data-role="menu"]')).toHaveAttribute('value', 'hiding');
    const s = hidingSection(page);

    // T11: a topic without rules says so, and its action opens the window.
    await expect(s.locator('tf-empty-state')).toHaveAttribute('title', 'Ten topik nie ma zasad ukrywania danych', { timeout: 15000 });
    await expect(d.locator('[data-role="menu"] tf-tab#hiding')).not.toHaveAttribute('count', /.+/);
    await assertNoBannedWords(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-e-recepty.png'), fullPage: true });
    await s.locator('tf-empty-state [data-go="hiding-add"]').click();
    const win = hidingWindow(page);
    await expect(win).toHaveCount(1);
    await expect(win.locator('[data-role="subject"] select option', { hasText: `${groupName} (1 osoba)` })).toHaveCount(1, { timeout: 15000 });
    await win.locator('[data-role="subject"] select').selectOption({ label: `${groupName} (1 osoba)` });
    await expect(win.locator('tf-segmented[data-field]')).toHaveCount(4);
    await expect(win.locator('[data-role="source-note"]')).toContainText('Pola pochodzą ze wzoru wiadomości wizyta, wersja 3.');
    await expect(win.locator('[data-role="impact"]')).toContainText('Ustaw co najmniej jedno pole na „Ukryj”');
    await expect(win.locator('[data-act="save"]')).toHaveAttribute('disabled', '');
    await pickSegment(win, 'tf-segmented[data-field="lekarz"]', 'Ukryj');
    await expect(win.locator('[data-role="impact"]')).toContainText(`Dla „${groupName}” znikną z wiadomości pola: lekarz. Pozostałe pola z listy bez zmian.`);
    await windowFits(page, 'tf-window.tb-hiding-window', DESKTOP.width);
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-dodaj-wizyty.png') });
    await win.locator('[data-act="save"]').click();
    await expect(win).toHaveCount(0);
    await expect(s.locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', 'Zapisano zasadę');
    await expect(ruleRow(page, groupName)).toContainText('Grupa · 1 osoba');
    await expect(ruleRow(page, groupName)).toContainText('lekarz');
    await expect(ruleRow(page, groupName)).toContainText('Ukryj');
    await expect(ruleRow(page, groupName)).toContainText('Odczyt');
    await expect(s.locator('[data-role="count"] tf-chip')).toHaveAttribute('label', '1');
    await expect(d.locator('[data-role="menu"] tf-tab#hiding')).toHaveAttribute('count', '1');
    await expect(s.locator('[data-role="keys-note"]')).toContainText('zakresie odczytu');
    expect(await serverRules(page, instanceId, 'wizyty')).toEqual({ [`group:${groupId}:read`]: { fields: ['gabinet', 'pacjent', 'termin'], required: [] } });
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-dodano-wizyty.png'), fullPage: true });

    // Zmień: one more field hidden — said before saving, subject fixed.
    await ruleRow(page, groupName).locator('tf-button[data-act="change"]').click();
    await expect(win.locator('.tb-explain-box').first()).toContainText(groupName);
    await expect(fieldSeg(win, 'lekarz')).toHaveAttribute('value', 'hide');
    await expect(win.locator('[data-act="save"]')).toHaveAttribute('disabled', '');
    await pickSegment(win, 'tf-segmented[data-field="termin"]', 'Ukryj');
    await expect(win.locator('[data-role="impact"]')).toContainText(`Dla „${groupName}” znikną z wiadomości pola: termin. Pozostałe pola z listy bez zmian.`);
    await win.locator('[data-act="save"]').click();
    await expect(win).toHaveCount(0);
    await expect(s.locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', 'Zapisano zmianę zasady');
    expect((await serverRules(page, instanceId, 'wizyty'))[`group:${groupId}:read`].fields).toEqual(['gabinet', 'pacjent']);

    // Podgląd, jak widzi…: the newest message as the group reads it, and the audit entry.
    const audits = auditCount('bus.field_policy.preview');
    await s.locator('tf-button[data-go="hiding-preview"]').click();
    const preview = previewWindow(page);
    await expect(preview).toHaveCount(1);
    await expect(preview.locator('.tb-audit-banner')).toContainText('Ten podgląd zapisuje się w dzienniku audytu.');
    await expect(preview.locator('[data-role="offset"] input')).not.toHaveValue('', { timeout: 15000 });
    await expect(preview.locator('[data-role="subject"] select option', { hasText: groupName })).toHaveCount(1, { timeout: 15000 });
    await preview.locator('[data-role="subject"] select').selectOption({ label: `${groupName} (1 osoba)` });
    await preview.locator('[data-act="show"]').click();
    await expect(preview.locator('[data-role="summary"]')).toHaveText('Zasady ukryły 2 pola: lekarz, termin.', { timeout: 15000 });
    await expect(preview.locator('.tb-preview-record-head')).toContainText(`Tak widzi ją: grupa ${groupName}`);
    const shown = await preview.locator('[data-role="payload"]').innerText();
    expect(shown).toContain('"pacjent"');
    expect(shown).not.toContain('"lekarz"');
    expect(shown).not.toContain('"termin"');
    await expect(preview.locator('[data-role="applied"] .tb-right-row[data-field-action="hide"]')).toHaveCount(2);
    await expect(preview.locator('[data-role="limited"]')).toHaveCount(0);
    expect(auditCount('bus.field_policy.preview')).toBe(audits + 1);
    await windowFits(page, 'tf-window.tb-hiding-preview', DESKTOP.width);
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-podglad-wizyty.png') });

    // A rule for everyone covers the administrator too, so the group's preview
    // is narrower than what the group sees — and the window says so.
    await preview.locator('[data-act="close"]').click();
    await expect(preview).toHaveCount(0);
    await s.locator('tf-button[data-go="hiding-add"]').click();
    await pickSegment(win, '[data-role="kind"]', 'Wszyscy');
    await expect(win.locator('[data-role="pick-note"]')).toContainText('Tylko ją widzą systemy z kluczem API.');
    await pickSegment(win, 'tf-segmented[data-field="gabinet"]', 'Ukryj');
    await win.locator('[data-act="save"]').click();
    await expect(win).toHaveCount(0);
    await expect(ruleRow(page, 'Wszyscy')).toContainText('gabinet');
    await expect(s.locator('[data-role="keys-note"]')).toHaveText('');
    await s.locator('tf-button[data-go="hiding-preview"]').click();
    await expect(preview.locator('[data-role="offset"] input')).not.toHaveValue('', { timeout: 15000 });
    await expect(preview.locator('[data-role="subject"] select option', { hasText: groupName })).toHaveCount(1, { timeout: 15000 });
    await preview.locator('[data-role="subject"] select').selectOption({ label: `${groupName} (1 osoba)` });
    await preview.locator('[data-act="show"]').click();
    await expect(preview.locator('tf-alert[data-role="limited"]')).toHaveAttribute('title', 'Ten podgląd jest węższy niż widok wybranego podmiotu', { timeout: 15000 });
    await expect(preview.locator('tf-alert[data-role="limited"]')).toHaveAttribute('message', /Podgląd nigdy nie pokazuje więcej, niż widzisz sam/);
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-podglad-zawezony.png') });
    await preview.locator('[data-act="close"]').click();

    // Zapis: the group must send a patient and may not send a surgery.
    await s.locator('tf-button[data-go="hiding-add"]').click();
    await pickSegment(win, '[data-role="direction"]', 'Zapis');
    await expect(win.locator('[data-role="subject"] select option', { hasText: `${groupName} (1 osoba)` })).toHaveCount(1, { timeout: 15000 });
    await win.locator('[data-role="subject"] select').selectOption({ label: `${groupName} (1 osoba)` });
    await expect(fieldSeg(win, 'pacjent').locator('.tf-seg-opt')).toHaveText(['Dozwolone', 'Wymagane', 'Niedozwolone']);
    await pickSegment(win, 'tf-segmented[data-field="pacjent"]', 'Wymagane');
    await pickSegment(win, 'tf-segmented[data-field="gabinet"]', 'Niedozwolone');
    await expect(win.locator('[data-role="impact"]')).toContainText(`Wiadomość od „${groupName}”, w której jest którekolwiek z pól: gabinet, zostanie odrzucona i nie trafi do topiku.`);
    await win.locator('[data-act="save"]').click();
    await expect(win).toHaveCount(0);
    await expect(s.locator('[data-role="rules"] tbody tr')).toHaveCount(3);
    const written = ruleRow(page, groupName).filter({ hasText: 'Zapis' });
    await expect(written).toContainText('Wymagane');
    await expect(written).toContainText('Niedozwolone');
    expect((await serverRules(page, instanceId, 'wizyty'))[`group:${groupId}:write`]).toEqual({ fields: ['lekarz', 'pacjent', 'termin'], required: ['pacjent'] });
    await expect(s.locator('[data-role="legend"]')).toContainText('Odrzuć wiadomość');
    await expect(s.locator('[data-role="legend"]')).not.toContainText('Zahaszuj');
    await assertNoBannedWords(page);
    await assertNoOverflow(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-wizyty.png'), fullPage: true });

    // Usuń: what the rule does now, then it is gone; the last one brings the empty state back.
    await ruleRow(page, 'Wszyscy').locator('tf-button[data-act="remove"]').click();
    await expect(win).toHaveCount(0);
    const confirm = page.locator('tf-window.tb-access-window');
    await expect(confirm.locator('.tb-explain-box')).toContainText('Zasada odczytu dla: Wszyscy. Ukrywa pola: gabinet.');
    await expect(confirm.locator('[data-role="impact"]')).toContainText('Każdy, kto nie ma własnej zasady odczytu, zobaczy w topiku wizyty całe wiadomości.');
    await expect(confirm.locator('[data-role="impact"]')).toContainText('nie będą mogły czytać tego topiku');
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-usun-wszyscy.png') });
    await confirm.locator('[data-act="go"]').click();
    await expect(confirm).toHaveCount(0);
    await expect(s.locator('[data-role="notice"] tf-alert')).toHaveAttribute('title', 'Usunięto zasadę');
    await expect(s.locator('[data-role="rules"] tbody tr')).toHaveCount(2);
    for (const direction of ['Zapis', 'Odczyt']) {
      await ruleRow(page, groupName).filter({ hasText: direction }).locator('tf-button[data-act="remove"]').click();
      await confirm.locator('[data-act="go"]').click();
      await expect(confirm).toHaveCount(0);
    }
    await expect(s.locator('tf-empty-state')).toHaveAttribute('title', 'Ten topik nie ma zasad ukrywania danych', { timeout: 15000 });
    expect(await serverRules(page, instanceId, 'wizyty')).toEqual({});
  } finally {
    await clearRules(page, instanceId, 'wizyty');
    await iamCall(page, 'iamDeleteGroupRequest', { groupId });
  }
  expect(await serverRules(page, instanceId, 'wizyty')).toEqual({});
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U7 HL7 topic: the dictionary with plain names, a wrong address refused before the request, the typed field kept, the preview blanks the hidden fields', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  await clearRules(page, instanceId, 'wyniki-badan');
  try {
    await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wyniki-badan&section=hiding`);
    const s = hidingSection(page);
    await expect(s.locator('tf-empty-state')).toBeVisible({ timeout: 20000 });
    await s.locator('tf-button[data-go="hiding-add"]').first().click();
    const win = hidingWindow(page);
    await pickSegment(win, '[data-role="kind"]', 'Wszyscy');
    await expect(win.locator('[data-role="source-note"]')).toContainText('najczęstsze pola HL7 v2');
    expect(await win.locator('tf-segmented[data-field]').count()).toBeGreaterThanOrEqual(55);
    await expect(fieldSeg(win, 'PID-5').locator('xpath=ancestor::div[contains(@class,"tb-right-row")]')).toContainText('Imię i nazwisko pacjenta');
    await pickSegment(win, 'tf-segmented[data-field="PID-5"]', 'Ukryj');
    await pickSegment(win, 'tf-segmented[data-field="PID-8"]', 'Ukryj');
    const tags = win.locator('tf-tag-input[data-role="extra-shown"] input');
    await tags.fill('PID-0');
    await tags.press('Enter');
    await expect(win.locator('[data-role="impact"]')).toContainText('„PID-0” nie jest adresem pola HL7. Adres ma postać SEGMENT-numer, np. PID-5.');
    await expect(win.locator('[data-act="save"]')).toHaveAttribute('disabled', '');
    await win.locator('tf-tag-input[data-role="extra-shown"] tf-chip').first().evaluate((chip) => chip.dispatchEvent(new CustomEvent('remove')));
    await tags.fill('PID-31');
    await tags.press('Enter');
    await expect(win.locator('[data-role="impact"]')).toContainText('znikną z wiadomości pola: Imię i nazwisko pacjenta (PID-5), Płeć (PID-8).');
    await expect(win.locator('[data-role="impact"]')).toContainText('Pola spoza listy też znikną, chyba że wpiszesz je niżej.');
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-dodaj-hl7.png') });
    await win.locator('[data-act="save"]').click();
    await expect(win).toHaveCount(0);
    await expect(ruleRow(page, 'Wszyscy')).toContainText('PID-5');
    await expect(ruleRow(page, 'Wszyscy')).toContainText('Płeć');
    const rules = await serverRules(page, instanceId, 'wyniki-badan');
    const fields = rules['any:*:read'].fields;
    expect(fields).toContain('PID-31');
    expect(fields).toContain('PID-3');
    expect(fields).not.toContain('PID-5');
    expect(fields).not.toContain('PID-8');

    await s.locator('tf-button[data-go="hiding-preview"]').click();
    const preview = previewWindow(page);
    await pickSegment(preview, '[data-role="kind"]', 'Wszyscy');
    await expect(preview.locator('[data-role="offset"] input')).not.toHaveValue('', { timeout: 15000 });
    await preview.locator('[data-act="show"]').click();
    // The server keeps what is allowed, so the fields of the message the
    // dictionary does not list (MSH-11, OBR-1, …) are hidden together with the two chosen.
    await expect(preview.locator('[data-role="summary"]')).toContainText(/Zasady ukryły \d+ pól: .*PID-5.*PID-8/, { timeout: 15000 });
    await expect(preview.locator('[data-role="summary"]')).toContainText('MSH-11');
    await expect(preview.locator('[data-role="applied"] .tb-right-row[data-field-action="hide"]', { hasText: 'PID-5' })).toContainText('Imię i nazwisko pacjenta');
    const message = await preview.locator('[data-role="payload"]').innerText();
    expect(message).toContain('MSH|');
    expect(message).not.toContain('Kowalski');
    expect(message.split('\n').length).toBeGreaterThanOrEqual(4);
    await preview.locator('[data-act="close"]').click();
    expect(errors, errors.join('\n')).toEqual([]);
  } finally {
    await clearRules(page, instanceId, 'wyniki-badan');
  }
});

test('U7 a topic with binary content cannot get a rule, and the section says why', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  const name = `e2e-binarny-${RUN}`;
  await busCall(page, 'busTopicCreateRequest', { instanceId, name, options: { partitions: 1, contentType: 'application/octet-stream' } });
  try {
    await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=${name}&section=hiding`);
    const s = hidingSection(page);
    await expect(s.locator('tf-empty-state')).toHaveAttribute('title', 'Nie można dodać zasady w tym topiku', { timeout: 20000 });
    await expect(s.locator('tf-empty-state')).toHaveAttribute('message', /przenosi treść binarną/);
    await expect(s.locator('[data-role="add"]')).toHaveAttribute('disabled', '');
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-powiadomienia.png'), fullPage: true });
  } finally {
    await busCall(page, 'busTopicDeleteRequest', { instanceId, name });
  }
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U7 at 390x844: Ukrywanie danych from the section list, the rules as cards, the windows fill the phone', async ({ page }) => {
  const errors = trackErrors(page);
  await page.setViewportSize(PHONE);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  await clearRules(page, instanceId, 'wizyty');
  await busCall(page, 'busFieldPolicySetRequest', { instanceId, topic: 'wizyty', subjectType: 'any', subjectId: '*', direction: 'read', fields: ['pacjent', 'termin', 'gabinet'], requiredFields: [] });
  try {
    await page.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wizyty`);
    const d = detailSlot(page);
    await expect(d.locator('.tb-title')).toHaveText('wizyty', { timeout: 20000 });
    await d.locator('[data-role="pick"] select').selectOption('hiding');
    await expect.poll(() => hashParams(page).section).toBe('hiding');
    const s = hidingSection(page);
    await expect(s.locator('[data-role="rules"] tbody tr').first()).toBeVisible({ timeout: 15000 });
    await expect(s.locator('[data-role="rules"] tbody tr').first()).toContainText('lekarz');
    await assertNoOverflow(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-390.png'), fullPage: true });
    await s.locator('tf-button[data-go="hiding-add"]').click();
    const win = hidingWindow(page);
    await expect(win.locator('tf-segmented[data-field]').first()).toBeVisible({ timeout: 15000 });
    await windowFits(page, 'tf-window.tb-hiding-window', PHONE.width);
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-dodaj-390.png') });
    await win.locator('[data-act="cancel"]').click();
    await expect(win).toHaveCount(0);
    await s.locator('tf-button[data-go="hiding-preview"]').click();
    const preview = previewWindow(page);
    await expect(preview.locator('[data-role="offset"] input')).not.toHaveValue('', { timeout: 15000 });
    await windowFits(page, 'tf-window.tb-hiding-preview', PHONE.width);
    await settled(page);
    await page.screenshot({ path: path.join(SHOTS, 'tp-ukrywanie-podglad-390.png') });
    await preview.locator('[data-act="close"]').click();
  } finally {
    await clearRules(page, instanceId, 'wizyty');
  }
  expect(errors, errors.join('\n')).toEqual([]);
});

test('U7 without administration: no Ukrywanie danych in the menu or the section list, and an address naming it opens Stan', async ({ page, browser }) => {
  await page.setViewportSize(DESKTOP);
  await login(page);
  const instanceId = await openInstance(page, 'Produkcja');
  // A reader: bus.read on this instance, no administration of any topic.
  const readerId = await ensureUser(page, 'tomasz', 'Tomasz-czyta-1', 'Tomasz Nowak');
  await iamCall(page, 'addonPermissionSetRequest', { addonId: instanceId, subjectType: 'user', subjectId: readerId, permissionId: 'bus.read', grantMode: 'allow' });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, locale: 'pl-PL', viewport: DESKTOP });
  const rp = await context.newPage();
  const readerErrors = trackErrors(rp);
  try {
    await rp.addInitScript(() => {
      localStorage.setItem('tentaflow_lang', 'pl');
      document.addEventListener('DOMContentLoaded', () => {
        const st = document.createElement('style');
        st.textContent = '.update-overlay{display:none!important}';
        document.head.appendChild(st);
      });
    });
    await loginAsAdmin(rp, { port: PORT, username: 'tomasz', password: 'Tomasz-czyta-1' });
    await rp.goto(`https://127.0.0.1:${PORT}/#/tentabus?instance=${instanceId}&tab=topics&topic=wyniki-badan&section=hiding`);
    const d = rp.locator('#tb-panel > [data-tb-view-slot="detail"]');
    await expect(d.locator('.tb-title')).toHaveText('wyniki-badan', { timeout: 20000 });
    await expect(d.locator('[data-role="menu"]')).toHaveAttribute('value', 'state');
    await expect(d.locator('[data-role="menu"] tf-tab#hiding')).toBeHidden();
    await expect(d.locator('[data-section="hiding"]')).toBeHidden();
    expect(await d.locator('[data-section="hiding"]').evaluate((el) => el.children.length)).toBe(0);
    const options = await d.locator('[data-role="pick"] select option').allTextContents();
    expect(options).not.toContain('Ukrywanie danych');
    expect(readerErrors.filter((e) => !/PolicyDenied|permission_denied|protocol error/i.test(e)), readerErrors.join('\n')).toEqual([]);
  } finally {
    await context.close();
  }
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
