// =============================================================================
// Plik: tentanas-snapshots.spec.js
// Opis: Rzeczywiste kliknięcia potwierdzeń ochrony snapshotów; transport jest fixture.
// Przykład: npx playwright test --config=tentanas.playwright.config.js tentanas-snapshots
// =============================================================================

const { test, expect } = require('@playwright/test');
const fs = require('node:fs/promises');
const path = require('node:path');

const root = path.resolve(__dirname, '../../tentaflow-core/www');
const artifacts = path.resolve(process.env.TENTANAS_E2E_ARTIFACTS || path.resolve(__dirname, '../../../new_apps/reviews/artifacts/T10'));
const errors = new WeakMap();

test.use({ viewport: { width: 1440, height: 1080 } });
test.beforeEach(async ({ page }) => {
  const messages = [];
  errors.set(page, messages);
  page.on('pageerror', error => messages.push(error.message));
  page.on('console', message => { if (message.type() === 'error') messages.push(message.text()); });
  const index = await fs.readFile(path.join(root, 'index.html'), 'utf8');
  const sprite = index.match(/<svg[^>]*(?:data-role="sprite"|aria-hidden="true")[\s\S]*?<\/svg>/)?.[0];
  expect(sprite).toContain('id="i-lock"');
  await page.route('http://127.0.0.1:18764/**', async route => {
    const pathname = new URL(route.request().url()).pathname;
    if (pathname === '/') {
      await route.fulfill({ contentType: 'text/html', body: `<!doctype html><html lang="pl"><head><meta charset="utf-8"><link rel="icon" href="data:,">${['controls', 'style', 'compat', 'install-wizard', 'tentanas'].map(name => `<link rel="stylesheet" href="/css/${name}.css">`).join('')}</head><body>${sprite}<main class="nas-root" id="snapshots" style="padding:24px"></main></body></html>` });
      return;
    }
    const file = path.resolve(root, '.' + decodeURIComponent(pathname));
    if (!file.startsWith(root + path.sep)) { await route.fulfill({ status: 403 }); return; }
    await route.fulfill({ path: file });
  });
  await page.goto('http://127.0.0.1:18764/');
  await page.evaluate(async () => {
    localStorage.setItem('tentaflow_lang', 'pl');
    const { I18n } = await import('/js/i18n.js');
    await I18n.init();
    window.snapshotsModule = await import('/js/modules/tentanas/snapshots.js');
    window.calls = [];
    window.snapshot = { name: 'tank/home@przed-migracja', shortName: 'przed-migracja', dataset: 'tank/home',
      createdAt: '2026-09-01 00:00:00', origin: 'manual', holds: 1, clones: [], usedBytes: 1048576 };
    window.fixtureScreen = {
      isAdmin: true, disposed: false, withSudo: async fn => fn(null),
      nas: async (kind, payload) => {
        window.calls.push({ kind, payload });
        if (kind === 'tentaNasSnapshotsListRequest') return { snapshots: [window.snapshot], total: 1, totalUsedBytes: 1048576 };
        if (kind === 'tentaNasSnapshotSchedulesListRequest') return { schedules: [] };
        if (kind === 'tentaNasSharesListRequest') return { shares: [] };
        if (kind === 'tentaNasSnapshotCreateRequest' || kind === 'tentaNasSnapshotDestroyRequest') return {};
        throw new Error('Nieoczekiwane żądanie ' + kind);
      },
    };
  });
});

test.afterEach(async ({ page }, testInfo) => {
  await testInfo.attach('konsola', { body: JSON.stringify(errors.get(page)), contentType: 'application/json' });
  expect(errors.get(page)).toEqual([]);
});

test('ochrona 90 dni: anuluj nie wysyła, osobne potwierdzenie wysyła dokładnie raz', async ({ page }) => {
  await page.evaluate(() => window.snapshotsModule.openSnapshotNowDialog(window.fixtureScreen, { dataset: 'tank/home', onDone() {} }));
  const form = page.locator('tf-window').first();
  await form.locator('#nas-sn-name input').fill('manual-t10');
  await form.locator('#nas-sn-protect').click();
  await form.locator('#nas-sn-protect-days input').fill('90');
  await form.locator('[data-action="confirm"]').click();
  const confirmation = page.locator('tf-window').last();
  await expect(confirmation).toContainText('wymaga zgody drugiego administratora');
  expect(await page.evaluate(() => window.calls.length)).toBe(0);
  await confirmation.locator('[data-action="cancel"]').click();
  await expect(page.locator('tf-window')).toHaveCount(1);
  expect(await page.evaluate(() => window.calls.length)).toBe(0);
  await form.locator('[data-action="confirm"]').click();
  await expect(page.locator('tf-window')).toHaveCount(2);
  await expect(page.locator('tf-window').last()).toContainText('wymaga zgody drugiego administratora');
  await fs.mkdir(artifacts, { recursive: true });
  await page.screenshot({ path: path.join(artifacts, 'snapshot-protect-confirm.png'), animations: 'disabled' });
  await page.locator('tf-window').last().locator('[data-action="confirm"]').click();
  await expect.poll(() => page.evaluate(() => window.calls.length)).toBe(1);
  expect(await page.evaluate(() => window.calls[0])).toEqual({ kind: 'tentaNasSnapshotCreateRequest',
    payload: { dataset: 'tank/home', shortName: 'manual-t10', recursive: false, protectDays: 90, sudoPassword: null } });
  await expect(page.locator('tf-window')).toHaveCount(0);
});

test('usunięcie chronionego snapshotu wymaga jawnego potwierdzenia zapisu żądania', async ({ page }) => {
  await page.evaluate(() => window.snapshotsModule.drawSnapshots(window.fixtureScreen, document.querySelector('#snapshots'),
    { pool: 'tank', datasets: [{ name: 'tank/home', snapshotCount: 1 }] }));
  const remove = page.locator('#nas-snap-table [data-act="delete"]');
  await remove.click();
  await expect(page.locator('tf-window')).toContainText('Usunięcie zostanie tylko ZAPISANE');
  await page.locator('tf-window [data-action="cancel"]').click();
  await expect(page.locator('tf-window')).toHaveCount(0);
  expect(await page.evaluate(() => window.calls.filter(c => c.kind === 'tentaNasSnapshotDestroyRequest'))).toEqual([]);
  await remove.click();
  await expect(page.locator('tf-window')).toContainText('Usunięcie zostanie tylko ZAPISANE');
  await fs.mkdir(artifacts, { recursive: true });
  await page.screenshot({ path: path.join(artifacts, 'snapshot-delete-confirm.png'), animations: 'disabled' });
  await page.locator('tf-window [data-action="confirm"]').click();
  await expect.poll(() => page.evaluate(() => window.calls.filter(c => c.kind === 'tentaNasSnapshotDestroyRequest').length)).toBe(1);
  expect(await page.evaluate(() => window.calls.find(c => c.kind === 'tentaNasSnapshotDestroyRequest').payload))
    .toEqual({ names: ['tank/home@przed-migracja'], sudoPassword: null });
  await expect(page.locator('tf-window')).toHaveCount(0);
});
