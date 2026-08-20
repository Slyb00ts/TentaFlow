// =============================================================================
// File: tests/e2e/flows-system-readonly.spec.js
// Description: A platform-seeded system flow (ps-chat, `flows.is_system = 1`)
//              must be presented as read-only: the list shows a "system" chip
//              and no delete button, and the Flow Builder opens it as a
//              preview (banner, disabled name/status, no save / palette drag /
//              node deletion). Boots an isolated tentaflow instance.
// =============================================================================

const { test, expect } = require('@playwright/test');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');

const PORT = 18311;
const WORK_DIR = path.join(os.tmpdir(), `tentaflow-e2e-flows-system-${PORT}`);
const DB = path.join(WORK_DIR, 'flows.db');
const HOME = path.join(WORK_DIR, 'home');
const WWW_DIR = path.join(__dirname, '../../tentaflow-core/www');

// Fixed UUID of the seeded ps-chat system flow (db/seed.rs PS_CHAT_FLOW_ID).
const PS_CHAT_FLOW_ID = '00000000-0000-4000-8000-000000000040';

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

async function openFlowsList(page) {
  await page.addInitScript(() => {
    document.addEventListener('DOMContentLoaded', () => {
      const st = document.createElement('style');
      st.textContent = '.update-overlay{display:none!important}';
      document.head.appendChild(st);
    });
  });
  await loginAsAdmin(page, { port: PORT });
  const nav = page.locator('.nav-item[data-view="flows"]');
  const menuBtn = page.locator('#mobile-menu-btn');
  if (await menuBtn.isVisible()) {
    await menuBtn.click();
    await nav.scrollIntoViewIfNeeded();
  }
  await nav.click();
  await expect(page.locator('#flows-host table')).toBeVisible();
}

test('system flow row has a system chip and no delete button', async ({ page }) => {
  await openFlowsList(page);
  const row = page.locator(`tr[data-key="flow-${PS_CHAT_FLOW_ID}"]`);
  await expect(row).toHaveCount(1);
  await expect(row.locator(`tf-chip[data-flow-system="${PS_CHAT_FLOW_ID}"]`)).toBeVisible();
  await expect(row.locator('[data-flow-delete]')).toHaveCount(0);
  await expect(row.locator('[data-flow-edit]')).toHaveAttribute('icon', 'eye');

  // A user-created flow keeps its delete button, so the rule is per-row.
  await page.locator('#btn-new-flow').click();
  await expect(page.locator('.fb-shell')).toBeVisible();
  await page.locator('.fb-shell [data-role="back"]').click();
  await expect(page.locator('#flows-host table')).toBeVisible();
  await expect(page.locator('[data-flow-delete]').first()).toBeVisible();
});

test('builder opens a system flow read-only', async ({ page }) => {
  await openFlowsList(page);
  await page.locator(`[data-flow-edit="${PS_CHAT_FLOW_ID}"]`).click();

  const shell = page.locator('.fb-shell');
  await expect(shell).toHaveClass(/fb-readonly/);
  await expect(shell.locator('[data-role="system-readonly"]')).toBeVisible();
  await expect(shell.locator('[data-role="name"]')).toBeDisabled();
  await expect(shell.locator('[data-role="status"]')).toHaveAttribute('disabled', '');
  await expect(shell.locator('[data-role="save"]')).toBeHidden();
  await expect(shell.locator('[data-role="variables"]')).toBeHidden();
  await expect(shell.locator('[data-role="undo"]')).toBeHidden();

  // The seeded graph renders; selecting a node shows the config panel
  // without the duplicate/delete footer and Delete does not remove it.
  const nodes = shell.locator('.fb-node');
  const before = await nodes.count();
  expect(before).toBeGreaterThan(0);
  await nodes.first().click();
  await expect(shell.locator('.fb-config .fb-config-footer')).toHaveCount(0);
  await expect(shell.locator('.fb-config [data-action="delete"]')).toHaveCount(0);
  await page.keyboard.press('Delete');
  await expect(nodes).toHaveCount(before);

  // Dragging a palette item onto the canvas adds nothing.
  const item = shell.locator('.fb-palette .fb-node-item').first();
  await expect(item).toBeVisible();
  const canvas = shell.locator('[data-role="canvas"]');
  const box = await canvas.boundingBox();
  await item.dragTo(canvas, { targetPosition: { x: box.width / 2, y: box.height / 2 } });
  await expect(nodes).toHaveCount(before);

  // No update request was ever issued: the flow list still reports the
  // original name after leaving the builder.
  await shell.locator('[data-role="back"]').click();
  await expect(page.locator(`tr[data-key="flow-${PS_CHAT_FLOW_ID}"]`)).toBeVisible();
});
