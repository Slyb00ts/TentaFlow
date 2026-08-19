// =============================================================================
// File: tests/e2e/analytics.spec.js
// Description: End-to-end suite for the unified "Analityka" dashboard screen.
//              Boots an isolated tentaflow instance (own port, sqlite db and
//              TENTAFLOW_HOME), seeds fixtures/analytics-seed.sql through the
//              sqlite3 CLI and drives the UI the way an admin would: tabs,
//              filters, drill-downs, quota editor, pricing editor, CSV export.
//              Every scenario runs at a desktop and a mobile viewport.
// =============================================================================

const { test, expect } = require('@playwright/test');
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');

const PORT = 18299;
const WORK_DIR = path.join(os.tmpdir(), `tentaflow-e2e-analytics-${PORT}`);
const DB = path.join(WORK_DIR, 'analytics.db');
const HOME = path.join(WORK_DIR, 'home');
const SEED = path.join(__dirname, 'fixtures', 'analytics-seed.sql');
const WWW_DIR = path.join(__dirname, '../../tentaflow-core/www');

// Fixture ids (must match fixtures/analytics-seed.sql).
const MARTA_ID = '8d1f02aa-4c3e-4b8a-9f21-7a5d3e9bc771';
const MARTA_NAME = 'Marta Kowalczyk';
const QWEN_NAME = 'Qwen 3.8 27B AWQ';
const QWEN_ID = 'cyankiwi/Qwen3.8-27B-AWQ-INT4';
const HEX64 = /^[0-9a-f]{64}$/;

let server = null;

async function stopAndWait(proc) {
  if (!proc) return;
  const exited = new Promise((resolve) => proc.once('exit', resolve));
  stopBinary(proc);
  await Promise.race([exited, new Promise((r) => setTimeout(r, 10000))]);
}

// First start migrates the schema, then the fixture is applied offline and
// the binary is restarted on the seeded database.
test.beforeAll(async () => {
  test.skip(!binaryExists(), 'tentaflow binary not built (target_shared/{release-fast,release,debug})');
  fs.rmSync(WORK_DIR, { recursive: true, force: true });
  fs.mkdirSync(WORK_DIR, { recursive: true });
  const env = { TENTAFLOW_WWW_DIR: WWW_DIR };
  const first = startBinary({ port: PORT, db: DB, home: HOME, env });
  await waitForServer(PORT, 60000);
  await stopAndWait(first);
  execFileSync('/usr/bin/sqlite3', [DB], { input: fs.readFileSync(SEED) });
  server = startBinary({ port: PORT, db: DB, home: HOME, env, keepDb: true });
  await waitForServer(PORT, 60000);
});

test.afterAll(async () => {
  await stopAndWait(server);
  server = null;
});

// ---------------------------------------------------------------------------
// Page helpers
// ---------------------------------------------------------------------------

// Collects console errors + uncaught exceptions; a test fails on any of them.
function trackErrors(page) {
  const errors = [];
  page.on('console', (m) => { if (m.type() === 'error') errors.push(`[console] ${m.text().slice(0, 400)}`); });
  page.on('pageerror', (e) => errors.push(`[pageerror] ${e.message.slice(0, 400)}`));
  return errors;
}

async function openAnalytics(page) {
  // The frontend is served from disk and its asset hash differs from the
  // build embedded in the binary, which raises the "new version" overlay —
  // irrelevant for these tests, so it is hidden before the app boots.
  await page.addInitScript(() => {
    document.addEventListener('DOMContentLoaded', () => {
      const st = document.createElement('style');
      st.textContent = '.update-overlay{display:none!important}';
      document.head.appendChild(st);
    });
  });
  await loginAsAdmin(page, { port: PORT });
  const nav = page.locator('.nav-item[data-view="analytics"]');
  await expect(nav).toHaveCount(1);
  await expect(page.locator('.nav-item[data-view="token-usage"]')).toHaveCount(0);
  await expect(page.locator('.nav-item[data-view="model-metrics"]')).toHaveCount(0);
  // Mobile: the sidebar is a drawer translated off-screen until the menu
  // button opens it (the nav item reports "visible" even while off-screen).
  const menuBtn = page.locator('#mobile-menu-btn');
  if (await menuBtn.isVisible()) {
    await menuBtn.click();
    await expect(page.locator('body')).toHaveClass(/drawer-open/);
    // The sidebar is long; the item may sit below the fold of the drawer.
    await nav.scrollIntoViewIfNeeded();
  }
  await nav.click();
  await expect(page.locator('#an-root #an-tabs')).toBeVisible();
  await waitOverviewLoaded(page);
}

// The overview is loaded once the KPI shows a real value and the three top
// tables have rows.
async function waitOverviewLoaded(page) {
  await expect(page.locator('#an-kpi-tokens')).not.toHaveAttribute('value', '—');
  await expect(page.locator('#an-kpi-tokens')).toHaveAttribute('title', /\d/);
  await expect(page.locator('#an-top-users-table tbody tr').first()).toBeVisible();
  await expect(page.locator('#an-top-nodes-table tbody tr').first()).toBeVisible();
  await expect(page.locator('#an-chart tf-bar-chart svg .tf-chart__bar').first()).toBeAttached();
}

async function waitPanelIdle(page) {
  await expect(page.locator('#an-panel .an-state tf-spinner')).toHaveCount(0);
}

async function clickTab(page, id) {
  await page.locator(`#an-tabs tf-tab#${id}`).click();
  await expect(page.locator(`#an-tabs tf-tab#${id} > button`)).toHaveClass(/active/);
  await waitPanelIdle(page);
}

// Selects a tf-segmented option by its value (the component renders light-DOM
// buttons with data-value).
async function pickSegment(page, hostSelector, value) {
  const btn = page.locator(`${hostSelector} [data-value="${value}"]`);
  await btn.click();
}

async function selectValue(page, hostSelector, value) {
  await page.locator(`${hostSelector} select`).selectOption(value);
}

function kpiTokensExact(page) {
  return page.locator('#an-kpi-tokens').getAttribute('title');
}

async function expectNoOverflow(page, label) {
  const r = await page.evaluate(() => ({ sw: document.documentElement.scrollWidth, iw: window.innerWidth }));
  expect(r.sw, `${label}: horizontal overflow ${r.sw} > ${r.iw}`).toBeLessThanOrEqual(r.iw);
}

// Raw <button>/<input>/<select> in the light DOM of the analytics root are only
// allowed as the internals of a tf-* component.
async function expectNoRawControls(page) {
  const raw = await page.evaluate(() => {
    const root = document.querySelector('#an-root');
    if (!root) return ['no #an-root'];
    const out = [];
    for (const el of root.querySelectorAll('button, input, select, textarea')) {
      let p = el.parentElement;
      let insideTf = false;
      while (p && p !== root) {
        if (p.tagName.startsWith('TF-')) { insideTf = true; break; }
        p = p.parentElement;
      }
      if (!insideTf) out.push(`${el.tagName.toLowerCase()}#${el.id || ''}.${el.className || ''}`);
    }
    return out;
  });
  expect(raw, 'raw controls outside tf-* components').toEqual([]);
}

async function expectCellTitlesAreNames(page, tableSelector) {
  const titles = await page.locator(`${tableSelector} tbody .tf-table__cell-title`).allInnerTexts();
  expect(titles.length).toBeGreaterThan(0);
  for (const t of titles) expect(t.trim(), `raw id rendered as a title: ${t}`).not.toMatch(HEX64);
}

async function openDrillFromRow(page, tableSelector, text) {
  const row = page.locator(`${tableSelector} tbody tr`).filter({ hasText: text }).first();
  await expect(row).toBeVisible();
  await row.click();
  await expect(page.locator('.an-crumbs')).toBeVisible();
  await expect(page.locator('.an-hero-name')).toContainText(text);
  await waitPanelIdle(page);
  await expect(page.locator('#an-mk-tokens')).not.toHaveText('—');
}

async function crumbBack(page) {
  await page.locator('.an-crumbs a').first().click();
  await expect(page.locator('.an-crumbs')).toHaveCount(0);
  await waitPanelIdle(page);
}

// ---------------------------------------------------------------------------
// Scenarios (shared by both viewports)
// ---------------------------------------------------------------------------

function scenarios({ mobile }) {
  test('navigation, overview KPIs, chart and top lists', async ({ page }) => {
    const errors = trackErrors(page);
    await openAnalytics(page);

    for (const id of ['an-kpi-tokens', 'an-kpi-requests', 'an-kpi-ttft', 'an-kpi-decode', 'an-kpi-errors', 'an-kpi-cost']) {
      await expect(page.locator(`#${id}`)).not.toHaveAttribute('value', '—');
    }
    expect(await page.locator('#an-chart tf-bar-chart svg .tf-chart__bar').count()).toBeGreaterThan(1);
    for (const t of ['an-top-models-table', 'an-top-users-table', 'an-top-nodes-table']) {
      expect(await page.locator(`#${t} tbody tr`).count()).toBeGreaterThan(0);
      await expectCellTitlesAreNames(page, `#${t}`);
    }
    await expect(page.locator('#an-top-users-table tbody')).toContainText(MARTA_NAME);
    await expect(page.locator('#an-top-nodes-table tbody')).toContainText('hazai');
    await expect(page.locator('#an-mesh-chip')).toContainText(/mesh/i);
    await expectNoRawControls(page);
    if (mobile) await expectNoOverflow(page, 'overview');
    expect(errors).toEqual([]);
  });

  test('every tab renders data; mobile keeps the layout inside the viewport', async ({ page }) => {
    const errors = trackErrors(page);
    await openAnalytics(page);

    await clickTab(page, 'users');
    expect(await page.locator('#an-users-table tbody tr').count()).toBeGreaterThanOrEqual(4);
    await expect(page.locator('#an-users-table tbody')).toContainText(MARTA_NAME);
    await expectCellTitlesAreNames(page, '#an-users-table');
    await expect(page.locator('#an-users-card-foot')).not.toBeEmpty();
    if (mobile) await expectNoOverflow(page, 'users');
    await expectNoRawControls(page);

    // Groups sub-view: keyed by group id, rendered by name with a member count.
    await pickSegment(page, '#an-users-sub', 'group');
    await waitPanelIdle(page);
    await expect(page.locator('#an-users-table tbody')).toContainText('Marketing');
    await expectCellTitlesAreNames(page, '#an-users-table');

    await clickTab(page, 'models');
    expect(await page.locator('#an-models-table tbody tr').count()).toBe(3);
    await expect(page.locator('#an-models-table tbody')).toContainText(QWEN_NAME);
    expect(await page.locator('#an-compare-table tbody tr').count()).toBeGreaterThan(0);
    await expectCellTitlesAreNames(page, '#an-models-table');
    if (mobile) await expectNoOverflow(page, 'models');
    await expectNoRawControls(page);

    await clickTab(page, 'nodes');
    expect(await page.locator('.an-node-card').count()).toBe(3);
    await expect(page.locator('#an-nodes-list')).toContainText('hazai');
    await expect(page.locator('#an-nodes-list')).toContainText('laptop-pw');
    expect(await page.locator('.an-node-card tbody tr').count()).toBeGreaterThan(0);
    for (const name of await page.locator('.an-node-name').allInnerTexts()) expect(name.trim()).not.toMatch(HEX64);
    if (mobile) await expectNoOverflow(page, 'nodes');
    await expectNoRawControls(page);

    await clickTab(page, 'limits');
    expect(await page.locator('#an-quotas-table tbody tr').count()).toBe(3);
    await expect(page.locator('#an-quotas-table tbody')).toContainText('Marketing');
    await expect(page.locator('#an-quotas-table tbody')).toContainText('Cała organizacja');
    await expect(page.locator('#an-quotas-table tbody')).toContainText('wyłączony');
    expect(await page.locator('#an-leases-table tbody tr').count()).toBe(2);
    await expect(page.locator('#an-leases-table tbody')).toContainText('biuro-mini');
    await expect(page.locator('#an-new-quota')).toBeVisible();
    if (mobile) await expectNoOverflow(page, 'limits');
    await expectNoRawControls(page);

    await clickTab(page, 'billing');
    expect(await page.locator('#an-bill-table tbody tr').count()).toBeGreaterThanOrEqual(4);
    await expect(page.locator('#an-bill-table tbody')).toContainText(MARTA_NAME);
    await expect(page.locator('#an-bill-table tbody')).toContainText('zł');
    expect(await page.locator('#an-struct-table tbody tr').count()).toBe(3);
    await expect(page.locator('#an-struct-table tbody')).toContainText('brak cennika');
    expect(await page.locator('#an-pricing-table tbody tr').count()).toBe(3);
    await expect(page.locator('#an-billing-note')).not.toBeEmpty();
    if (mobile) await expectNoOverflow(page, 'billing');
    await expectNoRawControls(page);

    expect(errors).toEqual([]);
  });

  test('drill-down into a user, a model and a node with breadcrumb back', async ({ page }) => {
    const errors = trackErrors(page);
    await openAnalytics(page);

    await clickTab(page, 'users');
    await openDrillFromRow(page, '#an-users-table', MARTA_NAME);
    await expect(page.locator('.an-crumbs')).toContainText(MARTA_NAME);
    await expect(page.locator('#an-hero-chips')).toContainText('Marketing');
    expect(await page.locator('#an-drill-chart tf-bar-chart svg .tf-chart__bar').count()).toBeGreaterThan(0);
    expect(await page.locator('#an-bd-model-table tbody tr').count()).toBeGreaterThan(0);
    expect(await page.locator('#an-periods-table tbody tr').count()).toBe(3);
    if (mobile) await expectNoOverflow(page, 'drill-user');
    await crumbBack(page);
    await expect(page.locator('#an-users-table tbody')).toContainText(MARTA_NAME);

    await clickTab(page, 'models');
    await openDrillFromRow(page, '#an-models-table', QWEN_NAME);
    await expect(page.locator('.an-crumbs')).toContainText(QWEN_NAME);
    if (mobile) await expectNoOverflow(page, 'drill-model');
    await crumbBack(page);
    await expect(page.locator('#an-models-table tbody')).toContainText(QWEN_NAME);

    await clickTab(page, 'nodes');
    await page.locator('.an-node-name', { hasText: 'hazai' }).first().click();
    await expect(page.locator('.an-crumbs')).toBeVisible();
    await expect(page.locator('.an-hero-name')).toContainText('hazai');
    await waitPanelIdle(page);
    await expect(page.locator('#an-mk-tokens')).not.toHaveText('—');
    if (mobile) await expectNoOverflow(page, 'drill-node');
    await crumbBack(page);
    await expect(page.locator('#an-nodes-list')).toContainText('hazai');

    await expectNoRawControls(page);
    expect(errors).toEqual([]);
  });

  test('filters reload automatically without a refresh button', async ({ page }) => {
    const errors = trackErrors(page);
    await openAnalytics(page);
    expect(await page.locator('#an-root tf-button[icon="refresh"]').count()).toBe(0);
    await expect(page.locator('#an-toolbar')).not.toContainText(/odśwież/i);

    const monthly = await kpiTokensExact(page);

    // Period: monthly -> daily swaps the key picker to a date input and
    // reloads (today's tokens are a fraction of the month).
    await pickSegment(page, '#an-f-period', 'daily');
    await expect(page.locator('#an-f-key-host tf-input#an-f-key')).toBeVisible();
    await expect.poll(() => kpiTokensExact(page)).not.toBe(monthly);
    await waitOverviewLoaded(page);
    const daily = await kpiTokensExact(page);

    // Period key: yesterday differs from today.
    const yesterday = new Date(Date.now() - 86400000).toISOString().slice(0, 10);
    await page.locator('#an-f-key input').fill(yesterday);
    await expect.poll(() => kpiTokensExact(page)).not.toBe(daily);
    await waitOverviewLoaded(page);

    // Hourly adds the hour select and narrows further.
    await pickSegment(page, '#an-f-period', 'hourly');
    await expect(page.locator('#an-f-key-host #an-f-hour')).toBeVisible();
    await expect.poll(() => kpiTokensExact(page)).not.toBe(daily);
    await waitOverviewLoaded(page);
    const hourly = await kpiTokensExact(page);
    const currentHour = await page.locator('#an-f-hour select').inputValue();
    const otherHour = currentHour === '10' ? '09' : '10';
    await selectValue(page, '#an-f-hour', otherHour);
    await expect.poll(() => kpiTokensExact(page)).not.toBe(hourly);

    await pickSegment(page, '#an-f-period', 'monthly');
    await expect(page.locator('#an-f-key-host tf-select#an-f-key')).toBeVisible();
    await expect.poll(() => kpiTokensExact(page)).toBe(monthly);
    await waitOverviewLoaded(page);

    // Previous month via the key select.
    const keys = await page.locator('#an-f-key select option').evaluateAll((o) => o.map((x) => x.value));
    await selectValue(page, '#an-f-key', keys[1]);
    await expect.poll(() => kpiTokensExact(page)).not.toBe(monthly);
    await waitOverviewLoaded(page);
    await selectValue(page, '#an-f-key', keys[0]);
    await expect.poll(() => kpiTokensExact(page)).toBe(monthly);

    // Node filter narrows the top nodes list to exactly that node.
    expect(await page.locator('#an-top-nodes-table tbody tr').count()).toBe(3);
    const nodeOptions = await page.locator('#an-f-node select option').evaluateAll((o) => o.map((x) => ({ value: x.value, label: x.textContent })));
    const biuro = nodeOptions.find((o) => o.label.includes('biuro-mini'));
    expect(biuro).toBeTruthy();
    await selectValue(page, '#an-f-node', biuro.value);
    await expect(page.locator('#an-top-nodes-table tbody tr')).toHaveCount(1);
    await expect(page.locator('#an-top-nodes-table tbody')).toContainText('biuro-mini');
    await expect.poll(() => kpiTokensExact(page)).not.toBe(monthly);
    await selectValue(page, '#an-f-node', '');
    await expect(page.locator('#an-top-nodes-table tbody tr')).toHaveCount(3);

    // Model filter narrows the top models list.
    expect(await page.locator('#an-top-models-table tbody tr').count()).toBe(3);
    await selectValue(page, '#an-f-model', QWEN_ID);
    await expect(page.locator('#an-top-models-table tbody tr')).toHaveCount(1);
    await expect(page.locator('#an-top-models-table tbody')).toContainText(QWEN_NAME);
    await selectValue(page, '#an-f-model', '');
    await expect(page.locator('#an-top-models-table tbody tr')).toHaveCount(3);

    // Filters persist across tabs: the users tab gets the same node filter.
    await selectValue(page, '#an-f-node', biuro.value);
    await expect(page.locator('#an-top-nodes-table tbody tr')).toHaveCount(1);
    await clickTab(page, 'users');
    await expect(page.locator('#an-f-node select')).toHaveValue(biuro.value);
    await expect(page.locator('#an-users-table tbody tr').first()).toBeVisible();
    expect(await page.locator('#an-users-table tbody tr').count()).toBeLessThan(5);

    expect(errors).toEqual([]);
  });

  test('limits: create, edit, disable and delete a quota through the modal', async ({ page }) => {
    const errors = trackErrors(page);
    await openAnalytics(page);
    await clickTab(page, 'limits');
    const rowsBefore = await page.locator('#an-quotas-table tbody tr').count();
    const martaRow = page.locator('#an-quotas-table tbody tr').filter({ hasText: MARTA_NAME });
    await expect(martaRow).toHaveCount(0);

    // Create.
    await page.locator('#an-new-quota').click();
    const modal = page.locator('tf-modal[open]');
    await expect(modal.locator('.tf-modal-card')).toBeVisible();
    await selectValue(page, 'tf-modal[open] #an-q-scope', 'user');
    await selectValue(page, 'tf-modal[open] #an-q-subject', MARTA_ID);
    await pickSegment(page, 'tf-modal[open] #an-q-period', 'daily');
    await page.locator('tf-modal[open] #an-q-max input').fill('1000000');
    await expect(page.locator('tf-modal[open] #an-q-max')).toHaveAttribute('hint', /1\s?mln/);
    await modal.locator('.tf-modal-footer tf-button', { hasText: 'Zapisz' }).click();
    await expect(page.locator('.toast', { hasText: 'Zapisano limit' })).toBeVisible();
    await expect(page.locator('tf-modal[open]')).toHaveCount(0);
    await expect(page.locator('#an-quotas-table tbody tr')).toHaveCount(rowsBefore + 1);
    await expect(martaRow).toHaveCount(1);
    await expect(martaRow).toContainText('1 mln');
    await expect(martaRow).toContainText('marta.k@firma.pl');
    await expect(martaRow).toContainText('dzienny');

    // Edit: bump the maximum.
    await martaRow.locator('tf-button').first().click();
    await expect(page.locator('tf-modal[open] .tf-modal-card')).toBeVisible();
    await expect(page.locator('tf-modal[open] #an-q-subject select')).toHaveValue(MARTA_ID);
    await page.locator('tf-modal[open] #an-q-max input').fill('2000000');
    await page.locator('tf-modal[open] .tf-modal-footer tf-button', { hasText: 'Zapisz' }).click();
    await expect(page.locator('tf-modal[open]')).toHaveCount(0);
    await expect(martaRow).toContainText('2 mln');
    await expect(page.locator('#an-quotas-table tbody tr')).toHaveCount(rowsBefore + 1);

    // Disable via the active toggle.
    await martaRow.locator('tf-button').first().click();
    await expect(page.locator('tf-modal[open] .tf-modal-card')).toBeVisible();
    const toggle = page.locator('tf-modal[open] #an-q-active');
    await expect(toggle).toHaveAttribute('checked', '');
    await toggle.click();
    await expect(toggle).not.toHaveAttribute('checked', '');
    await page.locator('tf-modal[open] .tf-modal-footer tf-button', { hasText: 'Zapisz' }).click();
    await expect(page.locator('tf-modal[open]')).toHaveCount(0);
    await expect(martaRow).toContainText('wyłączony');

    // Delete with confirmation.
    await martaRow.locator('tf-button').first().click();
    await expect(page.locator('tf-modal[open] .tf-modal-card')).toBeVisible();
    await page.locator('tf-modal[open] .tf-modal-footer tf-button', { hasText: 'Usuń' }).click();
    const confirm = page.locator('tf-modal[open]').filter({ hasText: 'Usunąć limit?' });
    await expect(confirm.locator('.tf-modal-card')).toBeVisible();
    await confirm.locator('tf-button[variant="primary"]').click();
    await expect(page.locator('.toast', { hasText: 'Usunięto limit' })).toBeVisible();
    await expect(page.locator('tf-modal[open]')).toHaveCount(0);
    await expect(martaRow).toHaveCount(0);
    await expect(page.locator('#an-quotas-table tbody tr')).toHaveCount(rowsBefore);

    if (mobile) await expectNoOverflow(page, 'limits-after-edit');
    expect(errors).toEqual([]);
  });

  test('billing: pricing edit persists across a tab reload', async ({ page }) => {
    const errors = trackErrors(page);
    await openAnalytics(page);
    await clickTab(page, 'billing');
    const qwenRow = page.locator('#an-pricing-table tbody tr').filter({ hasText: QWEN_NAME });
    await expect(qwenRow).toHaveCount(1);
    const input = qwenRow.locator('tf-input[data-field="prompt"] input');
    const before = await input.inputValue();
    // Distinct per viewport so both runs really change the stored value.
    const next = mobile ? '0.0190' : '0.0180';
    expect(before).not.toBe(next);
    await input.fill(next);
    await qwenRow.locator('tf-button', { hasText: 'Zapisz' }).click();
    await expect(page.locator('.toast', { hasText: 'Zapisano cennik' })).toBeVisible();
    await waitPanelIdle(page);
    await expect(qwenRow.locator('tf-input[data-field="prompt"] input')).toHaveValue(next);

    // Leave and come back: the value is served by the backend, not a cache.
    await clickTab(page, 'overview');
    await waitOverviewLoaded(page);
    await clickTab(page, 'billing');
    await expect(page.locator('#an-pricing-table tbody tr').filter({ hasText: QWEN_NAME }).locator('tf-input[data-field="prompt"] input')).toHaveValue(next);
    // Whisper has no pricing and stays flagged.
    await expect(page.locator('#an-pricing-table tbody tr').filter({ hasText: 'Whisper' })).toContainText('brak cennika');
    expect(errors).toEqual([]);
  });

  test('CSV export downloads a non-empty file with a header line', async ({ page }) => {
    const errors = trackErrors(page);
    await openAnalytics(page);
    const [download] = await Promise.all([
      page.waitForEvent('download'),
      page.locator('#an-export').click(),
    ]);
    expect(download.suggestedFilename()).toMatch(/^analytics-overview-.*\.csv$/);
    const body = fs.readFileSync(await download.path(), 'utf8').replace(/^﻿/, '');
    const lines = body.split('\n').filter((l) => l.trim());
    expect(lines.length).toBeGreaterThanOrEqual(2);
    expect(lines[0]).toContain('Model');
    expect(lines[0].split(';').length).toBeGreaterThanOrEqual(5);
    expect(body).toContain(QWEN_NAME);

    // Limits export carries the quota rows.
    await clickTab(page, 'limits');
    const [dl2] = await Promise.all([
      page.waitForEvent('download'),
      page.locator('#an-export').click(),
    ]);
    expect(dl2.suggestedFilename()).toBe('analytics-quotas.csv');
    const body2 = fs.readFileSync(await dl2.path(), 'utf8');
    expect(body2).toContain('Marketing');
    expect(errors).toEqual([]);
  });

  if (mobile) {
    test('mobile: tab strip scrolls and the last tab is reachable', async ({ page }) => {
      const errors = trackErrors(page);
      await openAnalytics(page);
      const strip = await page.locator('#an-tabs .tf-tabs-viewport > div').first().evaluate((s) => ({
        overflowX: getComputedStyle(s).overflowX,
        scrollWidth: s.scrollWidth,
        clientWidth: s.clientWidth,
      }));
      expect(['auto', 'scroll']).toContain(strip.overflowX);
      expect(strip.scrollWidth).toBeGreaterThan(strip.clientWidth);
      await clickTab(page, 'billing');
      await expect(page.locator('#an-tabs tf-tab#billing')).toBeInViewport();
      await expectNoOverflow(page, 'billing-tab-strip');
      expect(errors).toEqual([]);
    });
  }
}

// ---------------------------------------------------------------------------
// Viewports
// ---------------------------------------------------------------------------

test.describe('analytics desktop', () => {
  test.use({ viewport: { width: 1680, height: 1000 }, locale: 'pl-PL', timezoneId: 'UTC', acceptDownloads: true });
  scenarios({ mobile: false });
});

test.describe('analytics mobile', () => {
  test.use({
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true,
    locale: 'pl-PL',
    timezoneId: 'UTC',
    acceptDownloads: true,
  });
  scenarios({ mobile: true });
});
