// =============================================================================
// File: tests/e2e/frontend-cache-version.spec.js
// Description: E2E for the frontend asset-caching + build-hash version handshake.
//   Verifies the generated asset manifest / sw-version, cache headers, that the
//   client-loaded hash matches the server, that a matching build shows NO false
//   "new version" banner, and that a signalled update shows a dismissible banner.
// =============================================================================

const { test, expect } = require('@playwright/test');
const {
  startBinary, waitForServer, stopBinary, baseUrl, DEFAULT_PORT,
} = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');

const PORT = DEFAULT_PORT;
const BASE = baseUrl(PORT);
let proc;

test.beforeAll(async () => {
  proc = startBinary({ port: PORT });
  await waitForServer(PORT);
});

test.afterAll(async () => {
  stopBinary(proc);
});

const HEX16 = /^[0-9a-f]{16}$/;

test('asset manifest + sw-version are served with a consistent build hash', async ({ request }) => {
  const man = await request.get(`${BASE}/js/generated/asset-manifest.js`);
  expect(man.ok()).toBeTruthy();
  const manText = await man.text();
  const manHash = (manText.match(/ASSET_BUILD_HASH\s*=\s*"([0-9a-f]+)"/) || [])[1];
  expect(manHash).toMatch(HEX16);
  // The manifest lists real assets (root-absolute paths).
  expect(manText).toContain('/index.html');

  const sw = await request.get(`${BASE}/js/generated/sw-version.js`);
  expect(sw.ok()).toBeTruthy();
  const swText = await sw.text();
  const swHash = (swText.match(/__ASSET_BUILD_HASH\s*=\s*"([0-9a-f]+)"/) || [])[1];
  // Both artifacts come from the same build.rs pass — hashes MUST match.
  expect(swHash).toBe(manHash);
  // sw-version.js must never be HTTP-cached, else the browser misses updates.
  expect(sw.headers()['cache-control']).toContain('no-store');
});

test('sw.js is served with no-store', async ({ request }) => {
  const sw = await request.get(`${BASE}/sw.js`);
  expect(sw.ok()).toBeTruthy();
  expect(sw.headers()['cache-control']).toContain('no-store');
});

test('client-loaded asset hash equals the server hash (no false-positive banner)', async ({ page }) => {
  await loginAsAdmin(page, { port: PORT });

  // The hash baked into this build's asset-manifest, as the browser loads it.
  const clientHash = await page.evaluate(async () => {
    const m = await import('/js/generated/asset-manifest.js');
    return m.ASSET_BUILD_HASH;
  });
  expect(clientHash).toMatch(HEX16);

  const served = await page.evaluate(async () => {
    const r = await fetch('/js/generated/asset-manifest.js');
    const t = await r.text();
    return (t.match(/ASSET_BUILD_HASH\s*=\s*"([0-9a-f]+)"/) || [])[1];
  });
  expect(clientHash).toBe(served);

  // Same build on both sides => the update banner must NOT be visible.
  const banner = page.locator('.update-banner.visible');
  await expect(banner).toHaveCount(0);
});

test('signalled update shows a dismissible banner with per-hash dedup', async ({ page }) => {
  await loginAsAdmin(page, { port: PORT });

  // Soft update signal (different server hash) -> banner appears.
  await page.evaluate(() => {
    window.dispatchEvent(new CustomEvent('tf:update-available', {
      detail: { required: false, current: 'aaaaaaaaaaaaaaaa', server: 'bbbbbbbbbbbbbbbb' },
    }));
  });
  const banner = page.locator('.update-banner');
  await expect(banner).toHaveClass(/visible/);
  // Locale-tolerant (test env may be pl or en).
  await expect(banner.locator('.update-banner-title'))
    .toHaveText(/Nowa wersja aplikacji|New app version/);

  // Dismiss ("Później") hides it.
  await banner.locator('tf-button[data-action="dismiss"]').click();
  await expect(banner).not.toHaveClass(/visible/);

  // Re-signalling the SAME server hash stays dismissed.
  await page.evaluate(() => {
    window.dispatchEvent(new CustomEvent('tf:update-available', {
      detail: { required: false, server: 'bbbbbbbbbbbbbbbb' },
    }));
  });
  await expect(banner).not.toHaveClass(/visible/);

  // A NEW server hash surfaces the banner again.
  await page.evaluate(() => {
    window.dispatchEvent(new CustomEvent('tf:update-available', {
      detail: { required: false, server: 'cccccccccccccccc' },
    }));
  });
  await expect(banner).toHaveClass(/visible/);

  // Required update uses the stronger wording.
  await page.evaluate(() => {
    window.dispatchEvent(new CustomEvent('tf:update-available', {
      detail: { required: true, server: 'dddddddddddddddd' },
    }));
  });
  await expect(banner.locator('.update-banner-title'))
    .toHaveText(/Wymagana aktualizacja|Update required/);
});
