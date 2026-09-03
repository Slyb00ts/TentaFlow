// =============================================================================
// File: tests/e2e/agents-ui.spec.js
// Description: End-to-end suite for the redesigned "Agenci" module
//              (mockups/agenci-20260822 A01-A06). Boots an isolated tentaflow
//              instance, seeds fixtures/agents-runs-seed.sql through the
//              sqlite3 CLI and drives the screen the way an admin would:
//              the card list and its filters, the config-first agent detail
//              with its four tabs, the sticky save bar across a tab switch,
//              the tool catalog, the per-agent run history with its drill-in,
//              and the cross-agent runs tab.
//
//              The contract under test is STRUCTURAL: the detail view owns the
//              whole main column (no module header, no second tab strip), a
//              dirty draft survives moving between tabs, and every run list
//              names its agent and initiator instead of a bare UUID.
// =============================================================================

const { test, expect } = require('@playwright/test');
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');

const PORT = 18301;
// NOT under os.tmpdir(): /tmp is a RAM tmpfs on the dev machines and a booting
// instance unpacks ~1.5 GB of container bundles into TENTAFLOW_HOME — enough to
// fill it and truncate the extraction instead of failing loudly. `.runtime/` is
// gitignored and is where this repo keeps runtime data anyway (see
// helpers/spawn.js, which defaults its own homes there for the same reason).
const WORK_DIR = path.join(__dirname, '../../.runtime/e2e-agents-ui');
const DB = path.join(WORK_DIR, 'agents.db');
const HOME = path.join(WORK_DIR, 'home');
const SEED = path.join(__dirname, 'fixtures', 'agents-runs-seed.sql');
const WWW_DIR = path.join(__dirname, '../../tentaflow-core/www');

// Seeded roster entry the fixture attaches its runs to (db/seed.rs).
const AGENT_NAME = 'Generator scenariuszy';
const AGENT_SLUG = 'generator-scenariuszy';
const HEX_UUID = /^[0-9a-f-]{36}$/;

let server = null;

async function stopAndWait(proc) {
  if (!proc) return;
  const exited = new Promise((resolve) => proc.once('exit', resolve));
  stopBinary(proc);
  await Promise.race([exited, new Promise((r) => setTimeout(r, 10000))]);
}

// First start migrates and seeds the roster, then the run fixture is applied
// offline and the binary is restarted on the seeded database.
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

// Two init-time fixes, both applied before the app boots:
//   - `TENTAFLOW_WWW_DIR` hashes the frontend per request, so the instance
//     announces a "new version" modal over the whole UI whenever a source file
//     is newer than the page; it would swallow every click in this suite.
//   - a fresh instance defaults to English, while every string this suite
//     asserts is the Polish copy the mockups are written in.
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

async function openAgentsScreen(page) {
  await prepare(page);
  await page.goto(`https://127.0.0.1:${PORT}/`);
  await page.locator('#login-username input').first().waitFor({ state: 'visible', timeout: 20000 });
  await page.locator('#login-username input').first().fill('admin');
  await page.locator('#login-password input').first().fill('admin');
  await page.locator('#login-submit').click();
  await page.waitForSelector('aside', { timeout: 30000 });
  // `data-view` is the stable handle; the nav label is translated.
  await page.locator('.sidebar .nav-item[data-view="agents"]').first().click();
  await page.waitForSelector('#agents-grid-host .agent-card', { timeout: 20000 });
}

async function openAgentDetail(page, name = AGENT_NAME) {
  await page.locator('.agent-card').filter({ hasText: name }).first().click();
  await page.waitForSelector('#agent-detail-tabs tf-tab', { timeout: 15000 });
}

function detailTab(page, index) {
  return page.locator('#agent-detail-tabs tf-tab').nth(index);
}

// tf-table renders its rows in a shadow root, so cell text has to be read from
// inside it rather than through a light-DOM locator.
function tableText(page, id) {
  return page.locator(`#${id}`).evaluate((t) => t.shadowRoot.textContent || '');
}

function tableRowCount(page, id) {
  return page.locator(`#${id}`).evaluate((t) => t.shadowRoot.querySelectorAll('tbody tr').length);
}

test.describe('Agenci — lista (A01)', () => {
  test('cards carry the identity, meta and status the mockup shows', async ({ page }) => {
    await openAgentsScreen(page);
    const card = page.locator('.agent-card').filter({ hasText: AGENT_NAME }).first();
    await expect(card.locator('.agent-card-role')).toHaveText(AGENT_SLUG);
    await expect(card.locator('.agent-card-name tf-chip')).toBeVisible();
    await expect(card.locator('.agent-card-av')).toBeVisible();
    // Tool count and model always; the skills entry only when there are any,
    // so a seeded agent with no skills does not carry a "0 skills" line.
    const meta = card.locator('.agent-card-meta span');
    expect(await meta.count()).toBeGreaterThanOrEqual(2);
    await expect(meta.first()).toContainText(/\d+/);
    await expect(card.locator('.agent-card-meta')).not.toContainText(/\b0 umiejętnoś/);
  });

  test('filter chips carry counts and the routable filter narrows the grid', async ({ page }) => {
    await openAgentsScreen(page);
    const chips = page.locator('#agents-filter-enabled .tf-filter-chip');
    await expect(chips).toHaveCount(4);
    for (let i = 0; i < 4; i++) {
      await expect(chips.nth(i).locator('.tf-filter-chip-count')).toHaveText(/\d+/);
    }
    const total = await page.locator('.agent-card:not(.agent-card-add)').count();
    await chips.nth(3).click();
    await expect
      .poll(async () => page.locator('.agent-card:not(.agent-card-add)').count())
      .toBeLessThan(total);
    expect(await page.locator('.agent-card:not(.agent-card-add)').count()).toBeGreaterThan(0);
  });

  test('search narrows the grid to the matching agent', async ({ page }) => {
    await openAgentsScreen(page);
    await page.locator('#agents-search input').first().fill(AGENT_SLUG);
    await expect
      .poll(async () => page.locator('.agent-card:not(.agent-card-add)').count())
      .toBe(1);
    await expect(page.locator('.agent-card').first()).toContainText(AGENT_NAME);
  });
});

test.describe('Agenci — szczegóły agenta (A02-A05)', () => {
  test('detail owns the main column: no module header, no second tab strip', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    await expect(page.locator('#agents-page-header')).toBeHidden();
    await expect(page.locator('#agents-top-tabs')).toBeHidden();
    await expect(page.locator('.ag-breadcrumb')).toBeVisible();

    // The header card is one row: icon, meta, actions — not a vertical stack.
    const laidOut = await page.evaluate(() => {
      const h = document.querySelector('#ag-detail-header-host .tf-detail-header');
      const ico = h.querySelector('.big-ico').getBoundingClientRect();
      const meta = h.querySelector('.d-meta').getBoundingClientRect();
      const act = h.querySelector('.d-actions').getBoundingClientRect();
      return meta.left >= ico.right && act.left >= meta.left && Math.abs(ico.top - act.top) < 140;
    });
    expect(laidOut).toBe(true);

    await expect(page.locator('#agent-detail-tabs tf-tab')).toHaveCount(4);
    // The runs badge is populated before the tab is ever opened.
    await expect(detailTab(page, 3)).toHaveAttribute('count', /\d+/);
  });

  test('breadcrumb returns to the list and restores the module chrome', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    await page.locator('.ag-breadcrumb .crumb').first().click();
    await expect(page.locator('#agents-page-header')).toBeVisible();
    await expect(page.locator('#agents-top-tabs')).toBeVisible();
    await expect(page.locator('#agents-grid-host .agent-card').first()).toBeVisible();
  });

  test('leaving a dirty draft asks before discarding it', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    await page.locator('#ag-detail-body tf-input[data-cfg="display_name"] input').first()
      .fill(`${AGENT_NAME} — porzucony`);
    await expect(page.locator('#ag-detail-body .ag-save-bar')).toBeVisible();

    await page.locator('.ag-breadcrumb .crumb').first().click();
    const dialog = page.locator('tf-window').filter({ hasText: 'Niezapisane zmiany' });
    await expect(dialog).toBeVisible();
    // Cancelling keeps the operator where they were, draft intact.
    await dialog.locator('tf-button[data-action="cancel"]').click();
    await expect(page.locator('#agents-page-header')).toBeHidden();
    await expect(page.locator('#ag-detail-body tf-input[data-cfg="display_name"] input').first())
      .toHaveValue(`${AGENT_NAME} — porzucony`);
  });

  test('a dirty draft and its save bar survive a tab switch', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    const display = page.locator('#ag-detail-body tf-input[data-cfg="display_name"] input').first();
    await display.fill(`${AGENT_NAME} — szkic`);
    await expect(page.locator('#ag-detail-body .ag-save-bar')).toBeVisible();

    await detailTab(page, 1).click();
    await page.waitForSelector('#ag-detail-body .ag-tools-layout');
    await detailTab(page, 0).click();
    await page.waitForSelector('#ag-detail-body tf-input[data-cfg="display_name"]');

    await expect(page.locator('#ag-detail-body .ag-save-bar')).toBeVisible();
    await expect(page.locator('#ag-detail-body tf-input[data-cfg="display_name"] input').first())
      .toHaveValue(`${AGENT_NAME} — szkic`);
  });

  test('every loop limit is edited on a slider with a live value', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    const sliders = page.locator('#ag-detail-body tf-slider[data-cfg-slider]');
    await expect(sliders).toHaveCount(3);
    for (const field of ['max_iterations', 'max_subagents', 'max_spawn_depth']) {
      await expect(page.locator(`#ag-detail-body [data-slider-val="${field}"]`)).toHaveText(/\d+/);
    }
  });

  test('the prompt editor counts characters, tokens and template variables', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    const textarea = page.locator('#ag-detail-body tf-textarea[data-cfg="system_prompt"] textarea').first();
    await textarea.fill('Odpowiadaj w języku {{jezyk}} i trzymaj się faktów.');
    await expect(page.locator('#ag-detail-body [data-prompt-count]')).toContainText(/\d+/);
    await expect(page.locator('#ag-detail-body [data-prompt-vars] .ag-prompt-var')).toHaveText('{{jezyk}}');
  });

  test('tool catalog lists installed addons, catalog packages and core groups', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    await detailTab(page, 1).click();
    await page.waitForSelector('#ag-detail-body .ag-tools-layout');

    // A package with no instance stays visible, dashed and read-only (D5).
    const pkg = page.locator('#ag-detail-body [data-package-groups] .agents-tool-group').first();
    await expect(pkg).toHaveClass(/is-not-installed/);
    await expect(pkg.locator('.ag-install-hint')).toBeVisible();
    // Its install strip belongs to the opened package, not to every row.
    await expect(pkg.locator('.agents-tool-group-foot')).toBeHidden();
    await pkg.locator('[data-group-head]').click();
    await expect(pkg.locator('.agents-tool-group-foot tf-button')).toBeVisible();

    // core.* is grouped semantically and only the groups holding a pick open.
    const coreGroups = page.locator('#ag-detail-body [data-core-groups] .agents-tool-group');
    expect(await coreGroups.count()).toBeGreaterThan(1);
    const openCore = page.locator('#ag-detail-body [data-core-groups] .agents-tool-group.is-open');
    expect(await openCore.count()).toBeGreaterThan(0);
    expect(await openCore.count()).toBeLessThan(await coreGroups.count());

    // The summary names the picks, not only their count.
    await expect(page.locator('#ag-detail-body [data-tools-summary] tf-chip').first()).toBeVisible();
  });

  test('filtering the tool catalog reveals the matching rows', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    await detailTab(page, 1).click();
    await page.waitForSelector('#ag-detail-body .ag-tools-layout');
    await page.locator('#ag-detail-body [data-tools-search] input').first().fill('project_case_save');
    await expect
      .poll(async () => page.locator('#ag-detail-body .agents-tool-group-body:not([hidden]) .agents-tool-item').count())
      .toBeGreaterThan(0);
    await expect(page.locator('#ag-detail-body .agents-tool-item').filter({ hasText: 'core.project_case_save' }))
      .toHaveCount(1);
  });

  test('toggling a tool marks the draft dirty and updates the tab badge', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    await detailTab(page, 1).click();
    await page.waitForSelector('#ag-detail-body .ag-tools-layout');
    const before = await detailTab(page, 1).getAttribute('count');
    await page.locator('#ag-detail-body [data-core-groups] .agents-tool-group.is-open tf-toggle[data-tool]:not([checked])')
      .first().click();
    await expect(page.locator('#ag-detail-body .ag-save-bar')).toBeVisible();
    await expect(detailTab(page, 1)).not.toHaveAttribute('count', before ?? '');
  });

  test('the test tab pairs the sandbox chat with a live run panel and session stats', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    await detailTab(page, 2).click();
    await page.waitForSelector('#ag-detail-body .ag-test-cols');
    await expect(page.locator('#ag-detail-body .agents-pg-chat tf-chat-composer')).toBeVisible();
    await expect(page.locator('#ag-detail-body .agents-pg-run')).toBeVisible();
    await expect(page.locator('#ag-detail-body [data-pg-stats] .k')).toHaveCount(4);
    // The stop button only exists while a run is in flight.
    await expect(page.locator('#ag-detail-body [data-pg-stop]')).toBeHidden();
  });
});

test.describe('Agenci — przebiegi agenta (A05)', () => {
  test('the run table names the run, its span and its initiator', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    await detailTab(page, 3).click();
    await page.waitForSelector('#agent-runs-table');

    // The 90-day-old fixture row is outside the default 30-day window.
    expect(await tableRowCount(page, 'agent-runs-table')).toBe(4);
    await expect(page.locator('#agent-runs-table-footer')).toContainText(/4/);

    const text = await tableText(page, 'agent-runs-table');
    expect(text).toContain('Administrator');
    expect(text).toContain('płatności cyklicznych');
    // Iterations read as progress against the agent's own cap.
    expect(text).toMatch(/9\s*\/\s*\d+/);
  });

  test('the period filter reaches rows outside the default window', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    await detailTab(page, 3).click();
    await page.waitForSelector('#agent-runs-table');
    await page.locator('#agent-runs-filter-period select').first().selectOption('0');
    await expect.poll(async () => tableRowCount(page, 'agent-runs-table')).toBe(5);
    expect(await tableText(page, 'agent-runs-table')).toContain('#aaaa0005');
  });

  test('searching prompts narrows the run table', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    await detailTab(page, 3).click();
    await page.waitForSelector('#agent-runs-table');
    await page.locator('#agent-runs-search input').first().fill('logowania');
    await expect.poll(async () => tableRowCount(page, 'agent-runs-table')).toBe(1);
  });

  test('opening a run shows the summary card, its actions and the timeline', async ({ page }) => {
    await openAgentsScreen(page);
    await openAgentDetail(page);
    await detailTab(page, 3).click();
    await page.waitForSelector('#agent-runs-table');
    await page.locator('#agent-runs-table').evaluate((t) => t.shadowRoot.querySelector('tbody tr').click());

    const host = page.locator('#agent-runs-detail-host');
    await expect(host.locator('.agents-run-detail-head .title')).toBeVisible();
    await expect(host.locator('.agents-run-detail-grid')).toBeVisible();
    // Status, agent, exit, prompt, result, initiator, span, iterations, tokens, sub-runs.
    await expect(host.locator('.agents-kv .k')).toHaveCount(10);
    await expect(host.locator('[data-run-copy-id]')).toBeVisible();
    await expect(host.locator('[data-run-export]')).toBeVisible();
    await expect(host.locator('[data-run-timeline]')).toBeVisible();
    // Never a bare UUID as a value the operator is supposed to read.
    await expect(host.locator('.agents-kv .v').nth(1)).not.toHaveText(HEX_UUID);
  });
});

test.describe('Agenci — przebiegi wszystkich agentów (A06)', () => {
  async function openRunsTab(page) {
    await openAgentsScreen(page);
    await page.locator('#agents-top-tabs tf-tab').nth(1).click();
    await page.waitForSelector('#runs-table-host tf-table');
  }

  test('the module header, KPI strip and export follow the tab', async ({ page }) => {
    await openRunsTab(page);
    await expect(page.locator('#agents-title')).toHaveText(/Przebiegi/);
    await expect(page.locator('#runs-export')).toBeVisible();
    await expect(page.locator('#agents-new')).toBeHidden();
    await expect(page.locator('#runs-kpi-host tf-stat-card')).toHaveCount(4);

    await page.locator('#agents-top-tabs tf-tab').nth(0).click();
    await expect(page.locator('#agents-title')).toHaveText('Agenci');
    await expect(page.locator('#runs-export')).toBeHidden();
    await expect(page.locator('#agents-new')).toBeVisible();
  });

  test('the cross-agent table resolves the agent name instead of its UUID', async ({ page }) => {
    await openRunsTab(page);
    const text = await tableText(page, 'runs-tab-table');
    expect(text).toContain(AGENT_NAME);
    expect(text).toContain(AGENT_SLUG);
    expect(text).not.toMatch(/11111111-1111-4111-8111-111111111111/);
  });

  test('the agent and status filters narrow the cross-agent table', async ({ page }) => {
    await openRunsTab(page);
    const all = await tableRowCount(page, 'runs-tab-table');
    await page.locator('#runs-filter-status select').first().selectOption('failed');
    await expect.poll(async () => tableRowCount(page, 'runs-tab-table')).toBe(1);
    expect(all).toBeGreaterThan(1);
    expect(await tableText(page, 'runs-tab-table')).toContain('Scenariusze API /orders');
  });

  // Every seeded run settles before the UI ever sees it (an unsupervised
  // `running` row is closed as `interrupted` at boot), so this covers the
  // terminal affordance; the cancel branch of `buildRunRowActions` is reachable
  // only from a run this process actually started.
  test('a finished row offers the details affordance', async ({ page }) => {
    await openRunsTab(page);
    const labels = await page.locator('#runs-tab-table').evaluate((t) => [...t.shadowRoot.querySelectorAll('tbody tr')]
      .map((tr) => tr.lastElementChild?.textContent?.trim() ?? ''));
    expect(labels.length).toBeGreaterThan(0);
    expect(labels.every((l) => /Szczegóły/i.test(l))).toBe(true);
  });
});
