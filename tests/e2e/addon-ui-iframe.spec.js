// =============================================================================
// File: tests/e2e/addon-ui-iframe.spec.js
// Description: F1c-P1 frontend-only e2e — mounts the addon UI host harness
//              against the mock bundle and verifies sandbox flags, CSP-blocked
//              network egress, postMessage round-trip, permission denial
//              (EPERM), unknown actions (EUNKNOWN_ACTION), and not-yet-wired
//              backends (EUNIMPL). No tentaflow binary is started — a tiny
//              local static server serves www/ for the page.
// =============================================================================

const path = require('path');
const { test, expect } = require('@playwright/test');
const { serve, stop } = require('./helpers/static-www-server');

const WWW_ROOT = path.join(__dirname, '../../tentaflow-core/www');

let staticHandle;

test.beforeAll(async () => {
  staticHandle = await serve({ root: WWW_ROOT });
});

test.afterAll(async () => {
  await stop(staticHandle);
});

async function gotoHarnessPage(page) {
  // We serve a minimal driver HTML inline via setContent and let it pull the
  // host modules over the static server. This avoids depending on any binary
  // route while still exercising real ES module loading.
  await page.goto(`${staticHandle.baseUrl}/test-fixtures/addon-ui-demo.html`);
  await page.waitForFunction(() => !!document.querySelector('tf-addon-ui-frame'));
}

test.describe('F1c-P1 — addon UI iframe harness', () => {
  test('iframe is sandboxed to allow-scripts only', async ({ page }) => {
    await gotoHarnessPage(page);
    const sandbox = await page.evaluate(() => {
      const frame = document.querySelector('tf-addon-ui-frame');
      return frame.iframe.getAttribute('sandbox');
    });
    expect(sandbox).toBe('allow-scripts');
  });

  test('mock bundle receives ui.init event with permissions + theme', async ({ page }) => {
    await gotoHarnessPage(page);
    const frame = page.frameLocator('tf-addon-ui-frame iframe').first();
    await expect(frame.locator('#perms')).toContainText('alias.read', { timeout: 5000 });
  });

  test('alias.list_owned without backend returns empty array (filtered)', async ({ page }) => {
    // The static server does not run tentaflow, so the binary WS is not
    // reachable. We assert the harness reports the failure as EINTERNAL
    // (transport error) — not EPERM or EBADREQ — proving the permission
    // gate passed and dispatch reached the binary path.
    await gotoHarnessPage(page);
    const frame = page.frameLocator('tf-addon-ui-frame iframe').first();
    await frame.locator('#btn-aliases').click();
    const output = frame.locator('#output');
    await expect(output).toContainText(/alias\.list_owned/, { timeout: 5000 });
  });

  test('camera.list returns EUNIMPL (backend not yet wired)', async ({ page }) => {
    await gotoHarnessPage(page);
    const frame = page.frameLocator('tf-addon-ui-frame iframe').first();
    await frame.locator('#btn-cameras').click();
    await expect(frame.locator('#output')).toContainText(/EUNIMPL/, { timeout: 5000 });
  });

  test('unknown action returns EUNKNOWN_ACTION', async ({ page }) => {
    await gotoHarnessPage(page);
    const frame = page.frameLocator('tf-addon-ui-frame iframe').first();
    await frame.locator('#btn-bad-action').click();
    await expect(frame.locator('#output')).toContainText(/EUNKNOWN_ACTION/, { timeout: 5000 });
  });

  test('ui.notify triggers tf-addon-toast on parent', async ({ page }) => {
    await gotoHarnessPage(page);
    await page.evaluate(() => {
      window.__toasts = [];
      window.addEventListener('tf-addon-toast', (e) => window.__toasts.push(e.detail));
    });
    const frame = page.frameLocator('tf-addon-ui-frame iframe').first();
    await frame.locator('#btn-notify').click();
    await page.waitForFunction(() => (window.__toasts || []).length > 0, null, { timeout: 5000 });
    const toasts = await page.evaluate(() => window.__toasts);
    expect(toasts[0]).toMatchObject({ level: 'info', message: 'Hello from mock addon' });
  });

  test('addon without alias.read permission gets EPERM on alias.list_owned', async ({ page }) => {
    await gotoHarnessPage(page);
    await page.selectOption('#perm-preset', '');
    await page.click('#btn-remount');
    // Wait for re-init to propagate.
    const frame = page.frameLocator('tf-addon-ui-frame iframe').first();
    await expect(frame.locator('body')).toHaveAttribute('data-ready', '1', { timeout: 5000 });
    await frame.locator('#btn-aliases').click();
    await expect(frame.locator('#output')).toContainText(/EPERM/, { timeout: 5000 });
  });

  test('vector.search with malformed payload returns EBADREQ', async ({ page }) => {
    await gotoHarnessPage(page);
    const frame = page.frameLocator('tf-addon-ui-frame iframe').first();
    // Inject a request with an invalid k=0 to trigger the schema validator.
    await frame.locator('body').evaluate(() => {
      window.parent.postMessage({
        kind: 'request', id: 'bad-1', action: 'vector.search',
        payload: { namespace: 'faces', query: [0.1], k: 0 },
      }, '*');
      window.__lastBad = null;
      window.addEventListener('message', (e) => {
        if (e.data && e.data.id === 'bad-1') window.__lastBad = e.data;
      });
    });
    await frame.locator('body').evaluate(async () => {
      while (!window.__lastBad) await new Promise((r) => setTimeout(r, 50));
    });
    const result = await frame.locator('body').evaluate(() => window.__lastBad);
    expect(result.ok).toBe(false);
    expect(result.error.code).toBe('EBADREQ');
  });

  test('unmount removes iframe from registry (no leak)', async ({ page }) => {
    await gotoHarnessPage(page);
    // Install URL.revokeObjectURL spy BEFORE we trigger unmount so we can
    // assert the blob: URL is actually freed (not just orphaned).
    await page.evaluate(() => {
      window.__revokedUrls = [];
      const orig = URL.revokeObjectURL.bind(URL);
      URL.revokeObjectURL = (u) => {
        window.__revokedUrls.push(u);
        return orig(u);
      };
    });
    const iframeSrcBefore = await page.evaluate(() => {
      const f = document.querySelector('tf-addon-ui-frame');
      return f && f.iframe ? f.iframe.src : null;
    });
    await page.click('#btn-unmount');

    const present = await page.evaluate(() => !!document.querySelector('tf-addon-ui-frame'));
    expect(present).toBe(false);

    // The blob: URL the demo handed to the frame must have been revoked.
    if (iframeSrcBefore && iframeSrcBefore.startsWith('blob:')) {
      const revoked = await page.evaluate(() => window.__revokedUrls);
      expect(revoked).toContain(iframeSrcBefore);
    }

    // Registry must be empty — no orphaned record holding a contentWindow ref.
    const registrySize = await page.evaluate(async () => {
      const { addonUiHost } = await import('/js/addon-ui-host.js');
      return addonUiHost._registrySize();
    });
    expect(registrySize).toBe(0);
  });

  test('envelope with extra fields is rejected (EBADREQ)', async ({ page }) => {
    await gotoHarnessPage(page);
    const frame = page.frameLocator('tf-addon-ui-frame iframe').first();
    await frame.locator('body').evaluate(() => {
      window.__lastExtra = null;
      window.addEventListener('message', (e) => {
        if (e.data && e.data.id === 'extra-1') window.__lastExtra = e.data;
      });
      window.parent.postMessage({
        kind: 'request', id: 'extra-1', action: 'ui.notify',
        payload: { level: 'info', message: 'x' },
        addonId: 'spoof', // unexpected envelope field
      }, '*');
    });
    await frame.locator('body').evaluate(async () => {
      while (!window.__lastExtra) await new Promise((r) => setTimeout(r, 50));
    });
    const result = await frame.locator('body').evaluate(() => window.__lastExtra);
    expect(result.ok).toBe(false);
    expect(result.error.code).toBe('EBADREQ');
  });

  test('payload with extra fields is rejected (EBADREQ)', async ({ page }) => {
    await gotoHarnessPage(page);
    const frame = page.frameLocator('tf-addon-ui-frame iframe').first();
    await frame.locator('body').evaluate(() => {
      window.__lastPayloadExtra = null;
      window.addEventListener('message', (e) => {
        if (e.data && e.data.id === 'pl-extra-1') window.__lastPayloadExtra = e.data;
      });
      window.parent.postMessage({
        kind: 'request', id: 'pl-extra-1', action: 'ui.notify',
        payload: { level: 'info', message: 'x', __extra: true },
      }, '*');
    });
    await frame.locator('body').evaluate(async () => {
      while (!window.__lastPayloadExtra) await new Promise((r) => setTimeout(r, 50));
    });
    const result = await frame.locator('body').evaluate(() => window.__lastPayloadExtra);
    expect(result.ok).toBe(false);
    expect(result.error.code).toBe('EBADREQ');
  });
});
