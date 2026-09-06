// =============================================================================
// File: tests/e2e/tentaquant.spec.js
// Description: End-to-end suite for the TentaQuant screens Q01–Q08 plus the
//              results pair Q15/Q16. Boots an
//              isolated tentaflow instance (own port, sqlite db and
//              TENTAFLOW_HOME) and drives the UI the way an administrator
//              would: the laboratory list with nothing installed, installing a
//              laboratory from the Addons catalog, entering it (the single-lab
//              route rule of plan §19.8), the dashboard counters, creating a
//              project and opening its share window. Every scenario is checked
//              at a desktop and at a 390 px viewport, where nothing may scroll
//              sideways. The Runy tab (Q08) is driven through a REAL T1 run:
//              the Studio places it on Core, the stream fills the histogram and
//              the run then has to be in the laboratory's listing with its
//              detail, its metrics and its artifacts.
//
//              TentaQuant is the first MULTI-INSTANCE native app, so the suite
//              installs its own instance instead of seeding one: the instance
//              row, its database and its permission matrix are created by the
//              same path a user takes, which is the only way the route's
//              `?instance=` contract is exercised end to end.
//
//              The notebook (Q06) and the circuit Studio (Q07) compute in the
//              BROWSER: they need `www/js/quantum/quantum_glue.{js,wasm}`, which
//              tentaflow-core/build.rs generates. Without them the screens say
//              so instead of computing, and the T0 assertions below fail — which
//              is the intent: a build that cannot run a circuit is a broken one.
// =============================================================================

const { test, expect } = require('@playwright/test');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');

const PORT = 18311;
const WORK_DIR = path.join(os.tmpdir(), `tentaflow-e2e-tentaquant-${PORT}`);
const DB = path.join(WORK_DIR, 'tentaquant.db');
const HOME = path.join(WORK_DIR, 'home');
const WWW_DIR = path.join(__dirname, '../../tentaflow-core/www');

const LAB_NAME = 'Kwanty R&D';
const PROJECT_NAME = 'Grover 4-kubitowy';
const NOTEBOOK_NAME = 'Grover — notatnik';

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

async function open(page) {
  // The frontend is served from disk and its asset hash differs from the build
  // embedded in the binary, which raises the "new version" overlay — irrelevant
  // here, so it is hidden before the app boots.
  await page.addInitScript(() => {
    document.addEventListener('DOMContentLoaded', () => {
      const st = document.createElement('style');
      st.textContent = '.update-overlay{display:none!important}';
      document.head.appendChild(st);
    });
  });
  await loginAsAdmin(page, { port: PORT });
}

async function gotoTentaQuant(page) {
  await page.goto(`https://127.0.0.1:${PORT}/#/tentaquant`);
  await expect(page.locator('#tq-root')).toBeVisible();
}

// The whole point of the responsive pass: no screen may scroll sideways.
async function expectNoHorizontalOverflow(page) {
  const overflow = await page.evaluate(() => ({
    doc: document.documentElement.scrollWidth - document.documentElement.clientWidth,
    main: (() => {
      const m = document.getElementById('main');
      return m ? m.scrollWidth - m.clientWidth : 0;
    })(),
  }));
  expect(overflow.doc, 'document does not scroll sideways').toBeLessThanOrEqual(1);
  expect(overflow.main, '#main does not scroll sideways').toBeLessThanOrEqual(1);
}

// Opens the one project of the laboratory from the Projekty tab.
async function openProject(page) {
  await page.locator('#tq-tabs tf-tab#projects').click();
  await page.locator('.q-card[data-project] .qc-name').click();
  await expect(page.locator('.tq-project-header')).toBeVisible({ timeout: 30000 });
}

// Installs one TentaQuant instance through Addons → Katalog, which is exactly
// what the "+ Nowe laboratorium" tile points at.
async function installLaboratory(page, name) {
  await page.goto(`https://127.0.0.1:${PORT}/#/addons`);
  await page.locator('.addons-view-switch [data-view="catalog"]').click();
  const card = page.locator('[data-catalog-card="tentaquant"]');
  await expect(card).toBeVisible({ timeout: 30000 });
  await card.locator('[data-act="install"]').click();

  const win = page.locator('tf-window').last();
  await expect(win).toBeVisible();
  await win.locator('#inst-name input').fill(name);
  await win.locator('[data-action="confirm"]').click();
  await expect(win).toHaveCount(0, { timeout: 60000 });
}

// ---------------------------------------------------------------------------
// Q01 — the laboratory list
// ---------------------------------------------------------------------------

test.describe.serial('TentaQuant', () => {
  test('Q01 shows the empty laboratory list with the install tile and the membership hint', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);

    await expect(page.locator('.tq-page-title')).toHaveText('TentaQuant');
    // Nothing is installed yet: no tile but the one that creates a laboratory.
    await expect(page.locator('.q-card[data-lab]')).toHaveCount(0);
    await expect(page.locator('.card-new[data-new-lab]')).toHaveCount(1);
    await expect(page.locator('tf-empty-state')).toBeVisible();
    // Membership comes from the instance matrix in Addons — the screen says so.
    await expect(page.locator('#tq-root tf-alert')).toHaveAttribute('message', /quant\.read/);
    await expect(page.locator('.tq-table-footer')).toContainText('0');

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q01 at 390 px keeps every tile in one column and never scrolls sideways', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 820 });
    await open(page);
    await gotoTentaQuant(page);
    await expectNoHorizontalOverflow(page);
  });

  // -------------------------------------------------------------------------
  // Q02 — one laboratory is entered directly (plan §19.8)
  // -------------------------------------------------------------------------

  test('installing one laboratory makes the route open it instead of the list', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await installLaboratory(page, LAB_NAME);
    await gotoTentaQuant(page);

    // With exactly one laboratory there is no choice to make: the screen goes
    // straight in and the route names the instance it opened.
    await expect(page.locator('.tf-detail-header .d-name')).toContainText(LAB_NAME);
    await expect(page).toHaveURL(/#\/tentaquant\?instance=tentaquant-/);
    await expect(page.locator('.tf-detail-header .d-sub')).toContainText('tentaquant · instancja');

    // Only the tiers with a backend are claimed.
    await expect(page.locator('.tf-detail-header .tier')).toHaveCount(2);
    await expect(page.locator('.tf-detail-header .tier.t0')).toContainText('T0');
    await expect(page.locator('.tf-detail-header .tier.t1')).toContainText('T1');

    // Only the tabs whose screens exist.
    await expect(page.locator('#tq-tabs tf-tab')).toHaveCount(3);
    await expect(page.locator('#tq-tabs tf-tab#dashboard')).toBeVisible();
    await expect(page.locator('#tq-tabs tf-tab#projects')).toBeVisible();
    await expect(page.locator('#tq-tabs tf-tab#runs')).toBeVisible();

    // Four KPI cards, all of them numbers LabOverview returns.
    await expect(page.locator('.tq-kpi tf-stat-card')).toHaveCount(4);
    await expect(page.locator('.tq-kpi tf-stat-card').first()).toHaveAttribute('value', '0');
    // "Zacznij od" offers exactly the one action that exists today.
    await expect(page.locator('.start-card')).toHaveCount(1);

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('the breadcrumb goes back to the laboratory list', async ({ page }) => {
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('.tq-crumbs a').first().click();
    await expect(page.locator('.tq-page-title')).toHaveText('TentaQuant');
    await expect(page.locator('.q-card[data-lab]')).toHaveCount(1);
    await expect(page.locator('.q-card[data-lab]')).toContainText(LAB_NAME);
    // A laboratory only the installer can enter reads as "tylko Ty".
    await expect(page.locator('.q-card[data-lab] .qc-owner')).toContainText('tylko Ty');
  });

  // -------------------------------------------------------------------------
  // Q03 + Q04 — projects and the new-project window
  // -------------------------------------------------------------------------

  test('Q04 creates a private project that lands in "Moje projekty"', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);

    await page.locator('.tf-detail-header [data-act="new-project"]').click();
    const win = page.locator('tf-window.tq-modal');
    await expect(win).toBeVisible();
    // Only "pusty" is offered: examples and templates do not exist yet.
    await expect(win.locator('tf-choice-group').first().locator('tf-choice-card')).toHaveCount(1);
    // Private is the default.
    await expect(win.locator('#tq-np-visibility')).toHaveAttribute('value', 'private');

    await win.locator('#tq-np-name input').fill(PROJECT_NAME);
    await win.locator('[data-act="create"]').click();
    await expect(win).toHaveCount(0, { timeout: 30000 });

    await page.locator('#tq-tabs tf-tab#projects').click();
    const card = page.locator('.q-card[data-project]');
    await expect(card).toHaveCount(1);
    await expect(card).toContainText(PROJECT_NAME);
    await expect(card.locator('.qc-owner')).toContainText('Właściciel: Ty');
    await expect(card.locator('.qc-owner')).toContainText('prywatny');

    // Three sections, each with its head, and the footer summary under them.
    await expect(page.locator('.tq-section-head')).toHaveCount(3);
    await expect(page.locator('.tq-table-footer')).toContainText('1 projekt');

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q03 at 390 px stacks the sections without a sideways scroll', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 820 });
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('#tq-tabs tf-tab#projects').click();
    await expect(page.locator('.q-card[data-project]')).toHaveCount(1);
    await expectNoHorizontalOverflow(page);
  });

  // -------------------------------------------------------------------------
  // Q05 — sharing
  // -------------------------------------------------------------------------

  test('Q05 shows the owner, the role legend and the laboratory toggle', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('#tq-tabs tf-tab#projects').click();
    await page.locator('.q-card[data-project] [data-share]').click();

    const win = page.locator('tf-window.tq-share');
    await expect(win).toBeVisible();
    // The owner row is first and cannot be removed.
    const rows = win.locator('.tq-share-table tbody tr');
    await expect(rows).toHaveCount(1);
    await expect(rows.first()).toContainText('nie można usunąć');
    // All three project roles are explained.
    await expect(win.locator('.role-legend .rl')).toHaveCount(3);
    await expect(win.locator('.role-legend')).toContainText('Przeglądający');
    // A viewer computes only in the browser, without saving results.
    await expect(win.locator('.role-legend')).toContainText('T0');
    // Sharing never grants laboratory access.
    await expect(win.locator('tf-alert').last()).toHaveAttribute('message', /Addons/);
    // The lab-wide toggle exists and reflects the project's visibility.
    await expect(win.locator('#tq-share-lab')).toBeVisible();
    await expect(win.locator('#tq-share-lab')).not.toHaveAttribute('checked', '');
    // The person picker searches every TentaFlow account, so it belongs to
    // whoever may share and is there for an ordinary owner too.
    await expect(win.locator('#tq-share-search')).toBeVisible();

    await win.locator('[data-act="close"]').click();
    await expect(win).toHaveCount(0);

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q05 at 390 px keeps the share table inside its own scroller', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 820 });
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('#tq-tabs tf-tab#projects').click();
    await page.locator('.q-card[data-project] [data-share]').click();
    await expect(page.locator('tf-window.tq-share')).toBeVisible();
    await expectNoHorizontalOverflow(page);
    // On a phone the window is a bottom sheet: it sits on the bottom edge and
    // spans the full width instead of floating in the middle.
    const box = await page.locator('tf-window.tq-share .tf-window').boundingBox();
    expect(Math.round(box.x)).toBe(0);
    expect(Math.round(box.width)).toBe(390);
    expect(Math.round(box.y + box.height)).toBe(820);
  });

  test('a project card menu acts on the project without navigating into it', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('#tq-tabs tf-tab#projects').click();
    await page.locator('.q-card[data-project] [data-more]').click();
    await page.locator('[data-project-menu] tf-menu-item[action="share"]').click();
    // The menu sits inside the card, so its click must stop there: the share
    // window opens and the screen stays on the project list.
    await expect(page.locator('tf-window.tq-share')).toBeVisible();
    await expect(page).not.toHaveURL(/project=/);
    await expect(page.locator('.q-card[data-project]')).toHaveCount(1);

    expect(errors, errors.join('\n')).toEqual([]);
  });

  // -------------------------------------------------------------------------
  // Q06 + Q07 — one project: notebook, circuit Studio and files
  // -------------------------------------------------------------------------

  test('a project card opens the project with the five tabs that exist', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);

    await expect(page).toHaveURL(/project=/);
    await expect(page.locator('.tq-project-header .d-name')).toContainText(PROJECT_NAME);
    // Notatnik, Studio obwodów, Runy projektu, Wyniki, Pliki.
    await expect(page.locator('#tq-project-tabs tf-tab')).toHaveCount(5);
    await expect(page.locator('#tq-project-tabs tf-tab#notebook')).toBeVisible();
    await expect(page.locator('#tq-project-tabs tf-tab#studio')).toBeVisible();
    await expect(page.locator('#tq-project-tabs tf-tab#runs')).toBeVisible();
    await expect(page.locator('#tq-project-tabs tf-tab#results')).toBeVisible();
    await expect(page.locator('#tq-project-tabs tf-tab#files')).toBeVisible();
    // The laboratory level lives in the breadcrumb, not in a second tab bar.
    await expect(page.locator('.tq-crumbs a')).toHaveCount(2);

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q06 creates a notebook, adds a circuit cell and saves a new version', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);

    // A project starts without a notebook and offers to create one.
    await expect(page.locator('tf-empty-state')).toBeVisible();
    await page.locator('[data-act="create"]').click();
    const win = page.locator('tf-window.tq-modal');
    await expect(win).toBeVisible();
    await win.locator('#tq-name-input input').fill(NOTEBOOK_NAME);
    await win.locator('[data-action="confirm"]').click();
    await expect(win).toHaveCount(0, { timeout: 30000 });

    // The new notebook opens with the markdown cell it was seeded with.
    await expect(page.locator('.cells .cell')).toHaveCount(1);
    // The seed is a markdown heading, and the dashboard renderer draws it as
    // one — a cell that showed a literal '#' would not be a notebook.
    await expect(page.locator('.cells .cell .md h1')).toHaveText(NOTEBOOK_NAME);
    // Nothing is dirty right after loading.
    await expect(page.locator('[data-act="save"]')).toHaveAttribute('disabled', '');

    // Adding a circuit cell makes it dirty; saving mints version 2.
    await page.locator('.add-cell').last().locator('[data-add="circuit"]').click();
    await expect(page.locator('.cells .cell')).toHaveCount(2);
    await expect(page.locator('[data-act="save"]')).not.toHaveAttribute('disabled', '');
    await page.locator('[data-act="save"]').click();
    await expect(page.locator('[data-act="save"]')).toHaveAttribute('disabled', '', { timeout: 30000 });

    // The append-only history lists both versions.
    await page.locator('[data-act="versions"]').click();
    const versions = page.locator('tf-window.tq-modal .tq-share-table tbody tr');
    await expect(versions).toHaveCount(2);
    await page.locator('tf-window.tq-modal [data-action="close"]').click();

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q06 asks before another tab drops unsaved cells', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);

    // Unsaved work: one more cell, never written. The view object is the only
    // copy of it, and the next tab disposes that view.
    await expect(page.locator('.cells .cell')).toHaveCount(2, { timeout: 30000 });
    await page.locator('.add-cell').last().locator('[data-add="markdown"]').click();
    await expect(page.locator('.cells .cell')).toHaveCount(3);
    // The toolbar's "niezapisane zmiany" lives in the panel the next tab
    // replaces; the dot on the tab is what survives the click that needs it.
    await expect(page.locator('#tq-project-tabs tf-tab#notebook')).toHaveAttribute('dirty', '');

    // Cancelling keeps the column exactly as it was.
    await page.locator('#tq-project-tabs tf-tab#studio').click();
    const leave = page.locator('tf-window.tq-modal');
    await expect(leave).toBeVisible();
    await leave.locator('[data-action="cancel"]').click();
    await expect(leave).toHaveCount(0);
    await expect(page.locator('.nb-layout .cells .cell')).toHaveCount(3);

    // Discarding is the only way out that loses them, and it says so first.
    await page.locator('#tq-project-tabs tf-tab#studio').click();
    await page.locator('tf-window.tq-modal [data-action="discard"]').click();
    await expect(page.locator('#tq-studio-circuit')).toBeVisible({ timeout: 30000 });

    // Back on the notebook: the two cells the server holds, and nothing was
    // written on the way out.
    await page.locator('#tq-project-tabs tf-tab#notebook').click();
    await expect(page.locator('.cells .cell')).toHaveCount(2, { timeout: 30000 });
    await expect(page.locator('[data-act="save"]')).toHaveAttribute('disabled', '');

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q06 runs a circuit cell in the browser and shows the state beside it', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);

    const circuitCell = page.locator('.cells .cell').last();
    await expect(circuitCell.locator('tf-quantum-circuit')).toBeVisible();
    // The state panel follows the last circuit cell without any run.
    await expect(page.locator('#tq-nb-bloch tf-bloch-sphere')).toHaveCount(2, { timeout: 30000 });

    await circuitCell.locator('[data-act="run"]').click();
    // T0: counts come from this browser, drawn by tf-mime-output.
    await expect(circuitCell.locator('[data-out] tf-bar-chart')).toBeVisible({ timeout: 30000 });
    await expect(circuitCell.locator('[data-out] .oh')).toContainText('T0');

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q07 computes the state on every change and offers only the browser tier', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);
    await page.locator('#tq-project-tabs tf-tab#studio').click();

    await expect(page.locator('#tq-studio-circuit')).toBeVisible();
    // The live T0 state: one sphere per qubit of the starting circuit.
    await expect(page.locator('#tq-studio-bloch tf-bloch-sphere')).toHaveCount(2, { timeout: 30000 });
    // H + CX + measure is a Clifford circuit and the badge says so.
    await expect(page.locator('#tq-studio-clifford tf-chip')).toBeVisible();
    // Only the tier that exists is offered — no disabled QPU promises.
    await expect(page.locator('#tq-studio-target option')).toHaveCount(1);
    await expect(page.locator('#tq-studio-target')).toContainText('T0');
    // The three textual/graphical exports of §6.1 sit together in the toolbar.
    await expect(page.locator('[data-act="export-qasm"]')).toBeVisible();
    await expect(page.locator('[data-act="export-qiskit"]')).toBeVisible();
    await expect(page.locator('[data-act="export-svg"]')).toBeVisible();

    // Step mode brings the slider and the transport of the evolution animation.
    await page.locator('#tq-studio-mode button[data-value="step"]').click();
    await expect(page.locator('#tq-studio-steps')).toBeVisible();
    await expect(page.locator('#tq-studio-step')).toBeVisible();
    await page.locator('[data-act="play"]').click();
    await expect(page.locator('#tq-studio-step-value')).not.toHaveText('', { timeout: 10000 });

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q07 describes the selected gate and drops a stale histogram when the circuit changes', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);
    await page.locator('#tq-project-tabs tf-tab#studio').click();
    await expect(page.locator('#tq-studio-circuit')).toBeVisible();

    // Nothing selected: the card asks for a gate instead of standing empty.
    await expect(page.locator('#tq-studio-gate')).toContainText('Kliknij bramkę');
    await page.locator('#tq-studio-circuit [data-row="0"][data-column="0"]').click();
    await expect(page.locator('#tq-studio-gate-chip tf-chip')).toBeVisible();
    await expect(page.locator('[data-act="gate-duplicate"]')).toBeVisible();

    // A finished run, then an edit: the bars belong to the circuit that ran.
    await page.locator('[data-act="run"]').click();
    await expect(page.locator('#tq-studio-counts tf-bar-chart')).toBeVisible({ timeout: 30000 });
    await page.locator('[data-act="gate-duplicate"]').click();
    await expect(page.locator('#tq-studio-counts tf-bar-chart')).toHaveCount(0, { timeout: 10000 });

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q07 refuses a run it cannot sample and keeps the state panel beside it', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);
    await page.locator('#tq-project-tabs tf-tab#studio').click();
    await expect(page.locator('#tq-studio-circuit')).toBeVisible();

    // The same program without its classical register: it has a state and it
    // cannot be sampled. The engine refuses such a run in English, so the
    // screen has to answer the question itself, before the call.
    await page.locator('#tq-studio-mode button[data-value="text"]').click();
    await expect(page.locator('#tq-studio-text')).toBeVisible();
    await page.locator('#tq-studio-source').evaluate((editor) => {
      editor.value = 'OPENQASM 3.0;\ninclude "stdgates.inc";\n\nqubit[2] q;\n\nh q[0];\ncx q[0], q[1];\n';
    });
    await page.locator('[data-act="apply"]').click();
    // The resources card is what proves the new program landed: two qubits and
    // no classical bits at all.
    await expect(page.locator('#tq-studio-resources')).toContainText('2 (+0', { timeout: 30000 });
    await expect(page.locator('#tq-studio-bloch tf-bloch-sphere')).toHaveCount(2);

    await page.locator('[data-act="run"]').click();
    await expect(page.locator('#tq-studio-counts')).toContainText('pomiar', { timeout: 30000 });
    await expect(page.locator('#tq-studio-counts tf-bar-chart')).toHaveCount(0);
    // Nothing failed: the banner stays empty and the state panel is untouched.
    await expect(page.locator('#tq-studio-status tf-alert')).toHaveCount(0);
    await expect(page.locator('#tq-studio-bloch tf-bloch-sphere')).toHaveCount(2);

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q07 runs shots in the browser and saves the circuit into the project files', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);
    await page.locator('#tq-project-tabs tf-tab#studio').click();
    await expect(page.locator('#tq-studio-circuit')).toBeVisible();

    await page.locator('[data-act="run"]').click();
    await expect(page.locator('#tq-studio-counts tf-bar-chart')).toBeVisible({ timeout: 30000 });

    await page.locator('[data-act="save-qasm"]').click();
    await page.locator('#tq-project-tabs tf-tab#files').click();
    const rows = page.locator('#tq-file-table tbody tr');
    await expect(rows).toHaveCount(1, { timeout: 30000 });
    await expect(rows.first()).toContainText('.qasm');
    await expect(rows.first()).toContainText('OpenQASM 3');
    await expect(page.locator('.tq-table-footer')).toContainText('1 plik');

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q06 and Q07 at 390 px stack their panels without a sideways scroll', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 820 });
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);
    await expect(page.locator('.cells .cell').first()).toBeVisible();
    await expectNoHorizontalOverflow(page);

    await page.locator('#tq-project-tabs tf-tab#studio').click();
    await expect(page.locator('#tq-studio-circuit')).toBeVisible();
    await expectNoHorizontalOverflow(page);
    // Plan §13.5: a phone reads the circuit, it does not edit it.
    await expect(page.locator('#tq-studio-circuit')).toHaveAttribute('readonly', '');
    await expect(page.locator('#tq-studio-preview tf-chip')).toBeVisible();
  });

  // -------------------------------------------------------------------------
  // Q08 — Runy
  // -------------------------------------------------------------------------

  test('Q08 lists nothing before the first run and says so instead of drawing headers', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('#tq-tabs tf-tab#runs').click();

    await expect(page.locator('#tq-run-table')).toHaveCount(0);
    await expect(page.locator('#tq-panel tf-empty-state')).toContainText('Brak runów');
    // The filters of the mockup, minus the person filter: this administrator
    // holds `quant.instruct`, so that one IS here.
    await expect(page.locator('#tq-run-tier')).toBeVisible();
    await expect(page.locator('#tq-run-status')).toBeVisible();
    await expect(page.locator('.tq-table-footer')).toContainText('0 runów');
    // No promise of a comparison: `Run::Compare` is not on the wire.
    await expect(page.locator('#tq-panel')).not.toContainText('Porównaj');

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('a T1 run from the Studio streams its histogram and lands in the Runy tab', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);
    await page.locator('#tq-project-tabs tf-tab#studio').click();
    await expect(page.locator('#tq-studio-circuit')).toBeVisible();

    // The targets come off the wire: this browser and Core on this node. The
    // hint under the field is the server's own `auto` resolution.
    const target = page.locator('#tq-studio-target select');
    await expect(target.locator('option[value^="core:"]')).toHaveCount(1, { timeout: 30000 });
    await expect(page.locator('#tq-studio-target-hint')).toContainText('auto →');
    const core = await target.locator('option[value^="core:"]').first().getAttribute('value');
    await target.selectOption(core);

    await page.locator('[data-act="run"]').click();
    // The run is a laboratory run now: it has a row, a status and a link.
    await expect(page.locator('#tq-studio-run-status')).toBeVisible({ timeout: 60000 });
    await expect(page.locator('#tq-studio-counts tf-bar-chart')).toBeVisible({ timeout: 60000 });
    await expect(page.locator('#tq-studio-run-status')).toContainText('OK', { timeout: 60000 });
    // The evolution the node recorded drives the panel, step by step.
    await expect(page.locator('#tq-studio-state-tier')).toHaveText('T1 · Core');
    await expect(page.locator('#tq-studio-step-value')).toContainText('klatka');
    await expect(page.locator('[data-act="play"]')).toHaveAttribute('disabled', '');

    // The project's own tab is the same table, narrowed to this project.
    await page.locator('#tq-project-tabs tf-tab#runs').click();
    await expect(page.locator('#tq-run-table tbody tr')).toHaveCount(1, { timeout: 30000 });
    await expect(page.locator('#tq-run-table tbody tr').first()).toContainText('T1 · Core');
    await expect(page.locator('.tq-table-footer')).toContainText('1 run');

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('a run row opens its detail with the event line, the outputs and the artifacts', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('#tq-tabs tf-tab#runs').click();
    await expect(page.locator('#tq-run-table tbody tr')).toHaveCount(1, { timeout: 30000 });

    await page.locator('#tq-run-table tbody tr').first().click();
    await expect(page.locator('#tq-run-detail .run-detail')).toBeVisible({ timeout: 30000 });
    await expect(page).toHaveURL(/run=/);
    await expect(page.locator('.run-detail-head')).toContainText('OK');
    await expect(page.locator('.run-timeline .tl-item')).toHaveCount(4);
    await expect(page.locator('.run-detail')).toContainText('Zlecony');
    // The histogram of the run, drawn from the output the stream also carried.
    await expect(page.locator('#tq-run-outputs tf-mime-output')).not.toHaveCount(0);
    await expect(page.locator('.run-artifact')).not.toHaveCount(0);
    // It is the caller's own run, so both acts are offered.
    await expect(page.locator('[data-act="pin"]')).toBeVisible();

    await page.locator('[data-act="pin"]').click();
    await expect(page.locator('#tq-run-table tbody tr').first()).toContainText('przypięty', { timeout: 30000 });
    await expect(page.locator('.tq-table-footer')).toContainText('przypięte: 1');

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('the dashboard shows the run in "ostatnie runy" and in the week counter', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await expect(page.locator('.recent-row[data-run]')).toHaveCount(1, { timeout: 30000 });
    await expect(page.locator('.recent-row[data-run]')).toContainText('T1 · Core');
    await expect(page.locator('.tq-kpi tf-stat-card').nth(1)).toHaveAttribute('value', '1');

    // The row is a way into the run, which is the only reason it is clickable.
    await page.locator('.recent-row[data-run]').click();
    await expect(page.locator('#tq-run-detail .run-detail')).toBeVisible({ timeout: 30000 });

    expect(errors, errors.join('\n')).toEqual([]);
  });

  // -------------------------------------------------------------------------
  // Q16 — the project results gallery, and Q15 — the full-screen run result
  // -------------------------------------------------------------------------

  test('Q16 draws the run tile client-side and opens the full result view', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);
    await page.locator('#tq-project-tabs tf-tab#results').click();

    const tile = page.locator('.res-tile').first();
    await expect(tile).toBeVisible({ timeout: 30000 });
    // The thumbnail is SVG drawn from `runs.tile_json`; the server sends no
    // picture and there is no image endpoint to fetch one from.
    await expect(tile.locator('svg.rt-svg')).toBeVisible();
    await expect(tile).toContainText('T1 · Core');
    await expect(page.locator('.tq-table-footer')).toContainText('1 wynik');

    await tile.locator('.rt-title').click();
    await expect(page.locator('#tq-result-tabs')).toBeVisible({ timeout: 30000 });
    await expect(page).toHaveURL(/result=/);
    // Breadcrumb: TentaQuant › lab › projekt › Run r-…
    await expect(page.locator('.tq-crumbs tf-breadcrumb-item')).toHaveCount(4);
    await expect(page.locator('#tq-result-tabs tf-tab')).toHaveCount(5);
    await expect(page.locator('.res-rail')).toContainText('Ziarno losowe');

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q15 animates the recorded evolution and replays the measured shots', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);
    await page.locator('#tq-project-tabs tf-tab#results').click();
    await page.locator('.res-tile .rt-title').first().click();
    await expect(page.locator('#tq-evolution')).toBeVisible({ timeout: 30000 });

    // The label says where the frames came from, and never pretends otherwise.
    await expect(page.locator('#tq-evolution')).toContainText('z klatek kluczowych Core');
    await expect(page.locator('#tq-strip .tf-timeline__gate').first()).toBeVisible();
    await expect(page.locator('#tq-evo-bloch tf-bloch-sphere').first()).toBeVisible();

    // The playhead starts BEFORE the first gate, so the Wyjaśnij box says the
    // gate is still to come — never that it ran and changed nothing.
    await expect(page.locator('#tq-explain-evolution')).not.toContainText('nie zmieniła nic widocznego');

    // Dragging to the end of the recording fills the histogram with the run's
    // own counts — the whole shot budget, not a fresh browser sample.
    await page.evaluate(() => {
      const strip = document.querySelector('#tq-strip');
      strip.dispatchEvent(new CustomEvent('seek', { detail: { position: strip.steps.length } }));
    });
    await expect(page.locator('#tq-evo-shots')).toContainText('/');
    const filled = await page.evaluate(() => {
      const series = document.querySelector('#tq-evo-hist').series[0];
      return Object.values(series.counts).reduce((sum, v) => sum + v, 0);
    });
    expect(filled).toBeGreaterThan(0);

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q15 draws the state views, the histogram and the export preview', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);
    await page.locator('#tq-project-tabs tf-tab#results').click();
    await page.locator('.res-tile .rt-title').first().click();
    await expect(page.locator('#tq-result-tabs')).toBeVisible({ timeout: 30000 });

    await page.locator('#tq-result-tabs tf-tab#state').click();
    await expect(page.locator('tf-qsphere')).toBeVisible({ timeout: 30000 });
    await expect(page.locator('#tq-state-bloch tf-bloch-sphere').first()).toBeVisible();
    await expect(page.locator('tf-state-bars')).toBeVisible();
    await expect(page.locator('tf-entanglement-graph')).toBeVisible();
    // The "Wyjaśnij" sentence is generated from the state, not from a model.
    await expect(page.locator('#tq-explain-state')).not.toBeEmpty();

    await page.locator('#tq-result-tabs tf-tab#histogram').click();
    await expect(page.locator('#tq-hist .tf-hist__group').first()).toBeVisible({ timeout: 30000 });
    await expect(page.locator('.cmp-metric').first()).toContainText('TVD');

    await page.locator('#tq-result-tabs tf-tab#data').click();
    await expect(page.locator('[data-method]')).toContainText('# Method note');
    await expect(page.locator('[data-bib]')).toContainText('@misc{tentaquant-');
    await expect(page.locator('tf-checkbox[data-part]')).toHaveCount(6);

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q15 and Q16 at 390 px stack their panels without a sideways scroll', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 820 });
    await open(page);
    await gotoTentaQuant(page);
    await openProject(page);
    await page.locator('#tq-project-tabs tf-tab#results').click();
    await expect(page.locator('.res-tile').first()).toBeVisible({ timeout: 30000 });
    await expectNoHorizontalOverflow(page);

    await page.locator('.res-tile .rt-title').first().click();
    await expect(page.locator('#tq-evolution')).toBeVisible({ timeout: 30000 });
    // The circuit strip is as wide as the recording, so it scrolls inside its
    // own box; the page must not.
    await expectNoHorizontalOverflow(page);

    for (const tab of ['state', 'histogram', 'compare', 'data']) {
      await page.locator(`#tq-result-tabs tf-tab#${tab}`).click();
      await expect(page.locator('#tq-result-panel')).toBeVisible();
      await expectNoHorizontalOverflow(page);
    }
  });

  test('Q08 at 390 px scrolls its table inside the card, never the page', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 820 });
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('#tq-tabs tf-tab#runs').click();
    await expect(page.locator('#tq-run-table')).toBeVisible({ timeout: 30000 });
    await expectNoHorizontalOverflow(page);

    await page.locator('#tq-run-table tbody tr').first().click();
    await expect(page.locator('#tq-run-detail .run-detail')).toBeVisible({ timeout: 30000 });
    await expectNoHorizontalOverflow(page);
  });

  // -------------------------------------------------------------------------
  // A second laboratory brings the list back (plan §19.8, the other branch)
  // -------------------------------------------------------------------------

  test('with two laboratories the route opens the list again', async ({ page }) => {
    await open(page);
    await installLaboratory(page, 'Sandbox lokalny');
    await gotoTentaQuant(page);

    await expect(page.locator('.tq-page-title')).toHaveText('TentaQuant');
    await expect(page.locator('.q-card[data-lab]')).toHaveCount(2);
    await expect(page.locator('.tq-table-footer')).toContainText('2 laboratoria');

    // The list view renders the same rows through tf-table.
    await page.locator('#tq-lab-view option[value="list"]').click();
    await expect(page.locator('#tq-lab-table tbody tr')).toHaveCount(2);
  });
});
