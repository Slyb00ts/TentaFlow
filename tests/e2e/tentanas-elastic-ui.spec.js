// =============================================================================
// Plik: tentanas-elastic-ui.spec.js
// Opis: Rzeczywista powłoka i komponenty Elastic z kontrolowanym transportem.
// Przykład: playwright test --config=tentanas.playwright.config.js tentanas-elastic-ui
// =============================================================================

const { test, expect } = require('@playwright/test');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');

const root = path.resolve(__dirname, '../../tentaflow-core/www');
const design = path.resolve(process.env.TENTANAS_MOCKUP_ROOT || path.resolve(__dirname, '../../../new_apps'));
const artifacts = path.resolve(process.env.TENTANAS_E2E_ARTIFACTS || path.join(design, 'reviews/artifacts/E2-UI'));
let server;
let base;
const errors = new WeakMap();

test.beforeAll(async () => {
  await fs.mkdir(artifacts, { recursive: true });
  const index = await fs.readFile(path.join(root, 'index.html'), 'utf8');
  const sprite = index.match(/<svg[^>]*(?:data-role="sprite"|aria-hidden="true")[\s\S]*?<\/svg>/)?.[0];
  expect(sprite).toContain('id="i-target"');
  const html = `<!doctype html><html lang="pl"><head><meta charset="utf-8"><link rel="icon" href="data:,">${['controls', 'style', 'compat', 'install-wizard', 'tentanas'].map((name) => `<link rel="stylesheet" href="/css/${name}.css">`).join('')}</head><body style="height:auto;overflow:auto">${sprite}<main id="nas-root" class="nas-root" style="padding:24px"></main></body></html>`;
  server = http.createServer(async (req, res) => {
    const pathname = new URL(req.url, 'http://localhost').pathname;
    if (pathname === '/') { res.setHeader('Content-Type', 'text/html'); res.end(html); return; }
    const directory = pathname.startsWith('/mockups/') ? design : root;
    const file = path.resolve(directory, '.' + decodeURIComponent(pathname));
    if (!file.startsWith(directory + path.sep)) { res.writeHead(403).end(); return; }
    try {
      const data = await fs.readFile(file);
      const types = { '.js': 'text/javascript', '.json': 'application/json', '.css': 'text/css', '.html': 'text/html', '.svg': 'image/svg+xml', '.wasm': 'application/wasm', '.woff2': 'font/woff2' };
      res.setHeader('Content-Type', types[path.extname(file)] || 'application/octet-stream');
      res.end(data);
    } catch { res.writeHead(404).end(); }
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  base = `http://127.0.0.1:${server.address().port}`;
});

test.afterAll(async () => { await new Promise((resolve) => server.close(resolve)); });
test.beforeEach(async ({ page }) => {
  errors.set(page, []);
  page.on('pageerror', (error) => errors.get(page).push(error.message));
  page.on('console', (message) => { if (message.type() === 'error') errors.get(page).push(message.text()); });
});
test.afterEach(async ({ page }, info) => {
  await info.attach('konsola', { body: JSON.stringify(errors.get(page)), contentType: 'application/json' });
  expect(errors.get(page)).toEqual([]);
});

async function openElastic(page, { language = 'pl', zfsError = false, parity = 1, state = 'active', elevation = 'helper' } = {}) {
  await page.addInitScript(async ({ language, zfsError, parity, state, elevation }) => {
    if (document.readyState === 'loading') await new Promise((resolve) => document.addEventListener('DOMContentLoaded', resolve, { once: true }));
    localStorage.setItem('tentaflow_lang', language);
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    ApiBinary.action = async (kind) => {
      if (kind === 'mePreferencesUpdateRequest') return {};
      throw new Error(`Nieoczekiwane żądanie poza ekranem ${kind}`);
    };
    const { I18n } = await import('/js/i18n.js');
    await I18n.init();
    const { default: module } = await import('/js/modules/tentanas.js');
    ApiBinary.one = async (kind) => {
      if (kind === 'authMeRequest') return { role: 'admin' };
      if (kind === 'tentaNasNodesListRequest') return { localNodeId: 'helios', nodes: ['helios', 'other'].map((nodeId) => ({ nodeId, nodeName: nodeId, isLocal: nodeId === 'helios', instanceStatus: 'ready', disksTotal: 6, poolsTotal: 2, sharesTotal: 0, ramBytes: 4 * 1024 ** 3 })) };
      throw new Error(`Nieoczekiwane żądanie powłoki ${kind}`);
    };
    const GiB = 1024 ** 3;
    const disk = (name, sizeBytes = 32 * GiB) => ({ diskId: name, name, device: `/dev/${name}`, kind: 'hdd', model: 'Dysk testowy', serial: `ELASTIC-${name}`, sizeBytes, health: 'ok', usedBy: '' });
    const freeDisks = [disk('vdb'), disk('vdc'), disk('vdd', 40 * GiB), disk('vde', 40 * GiB)];
    const array = { name: 'media', kind: 'elastic-array', filesystem: 'xfs', createPolicy: 'mfs', enabled: true, state, stateDetail: '', unionPath: '/mnt/media', usableBytes: null, usedBytes: null, updatedAt: '2026-09-07T12:00:00Z',
      dataDisks: [{ ...disk('vdf'), name: 'd1', role: 'data', filesystem: 'xfs', mountpoint: '/mnt/tentanas-branches/media/data/d1', mounted: true, devicePresent: true, usedBytes: null, freeBytes: null }],
      parityDisks: parity ? [{ ...disk('vdg', 40 * GiB), name: 'p1', index: 1, role: 'parity', mountpoint: '/mnt/tentanas-branches/media/parity/1', mounted: true, devicePresent: true, usedBytes: null }] : [],
      protection: { status: parity ? 'unknown' : 'unprotected', protectedAsOf: null, movedUnsyncedBytes: null, faultTolerance: parity ? null : 0 },
      snapraid: { installed: true, configPath: parity ? '/etc/tentanas/snapraid-media.conf' : null, lastSync: null, lastScrub: null, parityErrors: null },
    };
    window.fixture = { array, zfsError, freeDisks, outcome: 'job', jobs: [], elevation };
    window.calls = [];
    const screen = Object.assign(Object.create(module), {
      root: document.querySelector('main'), timers: new Set(), disposed: false,
      nas: async (kind, payload) => {
        window.calls.push({ kind, payload });
        if (kind === 'tentaNasEnvironmentRequest') return { environment: { uptimeSecs: 86400, probedAt: '2026-09-07T12:00:00Z', features: [{ id: 'zfs', status: 'ok' }, { id: 'mergerfs', status: 'ok' }, { id: 'snapraid', status: 'ok' }], elevation: { mode: window.fixture.elevation, helperState: 'ok', coreUser: 'tentanas', coreVersion: 'test' } } };
        if (kind === 'tentaNasJobsListRequest') return { jobs: window.fixture.jobs };
        if (kind === 'tentaNasElevationArmRequest') return new Promise((resolve) => { window.finishArm = resolve; });
        if (kind === 'tentaNasApprovalsListRequest') return { approvals: window.fixture.approvals || [], settings: { enabled: true, ttlHours: 24, adminCount: 2, byDefault: true } };
        if (kind === 'tentaNasSchedulesListRequest') return { rows: [], smart: { enabled: false } };
        if (kind === 'tentaNasSnapshotSchedulesListRequest') return { schedules: [] };
        if (kind === 'tentaNasAccessLogRequest') return { entries: [], total: 0 };
        if (kind === 'tentaNasPoolsListRequest') {
          if (window.fixture.zfsError) throw new Error('Nie można odczytać ZFS');
          return { pools: [{ name: 'tank', kind: 'zfs', state: 'online', health: 'ok', layout: 'mirror', dataDisks: 2, faultTolerance: 1, sizeBytes: 64 * GiB, usableBytes: 32 * GiB, usedBytes: 8 * GiB, compressRatio: 1.2, compression: 'lz4', encryption: 'off', vdevs: [], scan: { kind: 'none', status: 'idle', progressPct: 0, errors: 0 }, datasetCount: 1, snapshotCount: 0 }], freeDisks };
        }
        if (kind === 'tentaNasElasticArraysListRequest') return { arrays: [window.fixture.array] };
        if (kind === 'tentaNasElasticArrayGetRequest') return { array: window.fixture.array };
        if (kind === 'tentaNasElasticCapabilitiesRequest') return { capabilities: { mergerfs: true, snapraid: true, filesystems: ['xfs', 'ext4'] }, freeDisks };
        if (kind === 'tentaNasDisksListRequest') return { disks: freeDisks };
        if (kind === 'tentaNasDiskGetRequest') return { disk: { ...disk(payload.diskId), path: `/dev/${payload.diskId}`, role: 'elastic_data', memberOf: 'media', rotational: true, transport: 'virtio', mountpoints: ['/mnt/tentanas-branches/media/data/d1'], io: {}, ioHistoryBps: [] }, attributes: [], selfTests: [], history: [], alerts: [], historyDays: 7 };
        if (kind === 'tentaNasElasticArrayPlanRequest') return { plan: { usableBytes: payload.dataDiskIds.length * 32 * GiB, refusals: [], warnings: [], wipedDevices: [...payload.dataDiskIds, ...payload.parityDiskIds].map((id) => `/dev/${id}`), unionPath: `/mnt/${payload.name}`, stepsPreview: 'mkfs → mount → mergerfs' } };
        if (kind === 'tentaNasElasticArrayCreateRequest' || kind === 'tentaNasElasticArrayRestoreRequest') {
          if (window.fixture.outcome === 'approval') return { approval: { requestId: 'approval-elastic', operation: kind.includes('Create') ? 'elastic_create' : 'elastic_restore', status: 'pending' } };
          if (window.fixture.outcome === 'unknown') throw new Error('Przerwane połączenie po wysłaniu');
          const job = { jobId: 'job-elastic', kind: kind.includes('Create') ? 'elastic_create' : 'elastic_restore', subject: payload.name, status: 'running', progressPct: 10, log: [] };
          window.fixture.jobs = [job];
          if (kind.includes('Create')) window.fixture.array = { ...array, name: payload.name, unionPath: `/mnt/${payload.name}` };
          return { job };
        }
        if (kind === 'tentaNasElasticArraySyncRequest' || kind === 'tentaNasElasticArrayScrubRequest') {
          const action = kind.includes('Sync') ? 'sync' : 'scrub';
          if (window.fixture.outcome === 'approval') return { approval: { requestId: `approval-${action}`, operation: `elastic_${action}`, status: 'pending' } };
          if (window.fixture.outcome === 'unknown') throw new Error('Przerwane połączenie po wysłaniu');
          const job = { jobId: `job-${action}`, kind: `elastic_${action}`, subject: payload.name, status: 'running', progressPct: 10, log: [] };
          window.fixture.jobs = [job];
          window.fixture.maintenance = action;
          window.fixture.array.snapraid.history = [{ kind: action, outcome: 'running', jobId: job.jobId, startedAt: '2026-09-08T12:00:00Z' }];
          return { job };
        }
        if (kind === 'tentaNasJobGetRequest') {
          if (window.fixture.completeRestore) {
            window.fixture.array.state = 'active';
            for (const disk of [...window.fixture.array.dataDisks, ...window.fixture.array.parityDisks]) disk.mounted = true;
          }
          if (window.fixture.maintenance && window.fixture.jobStatus !== 'running') {
            const run = { kind: window.fixture.maintenance, jobId: payload.jobId, operationId: `operation-${window.fixture.maintenance}`, startedAt: '2026-09-08T12:00:00Z', finishedAt: '2026-09-08T12:01:00Z', outcome: window.fixture.maintenanceOutcome || 'ok', totalBlocks: 100, checkedBlocks: window.fixture.maintenance === 'scrub' ? 68 : null, exitCode: 0, errorsFile: 0, errorsIo: 0, errorsData: 0 };
            if (run.outcome === 'refused') Object.assign(run, { exitCode: null, checkedBlocks: null, totalBlocks: null, errorsFile: null, errorsIo: null, errorsData: null, detail: 'unsynced_changes' });
            window.fixture.array.snapraid.history = [run];
            if (run.kind === 'sync' && run.outcome === 'ok') { window.fixture.array.snapraid.lastSync = run; window.fixture.array.protection.protectedAsOf = run.finishedAt; }
            if (run.kind === 'scrub' && run.outcome !== 'refused') window.fixture.array.snapraid.lastScrub = run;
            if (run.outcome === 'needs_attention' || run.outcome === 'failed') window.fixture.array.state = 'needs_attention';
          }
          return { job: { ...window.fixture.jobs[0], status: window.fixture.jobStatus || 'succeeded', progressPct: 100, log: ['Gotowe'] } };
        }
        throw new Error(`Nieoczekiwane żądanie ${kind}`);
      },
    });
    window.screenUnderTest = screen;
    await screen.mount({ node: 'helios', tab: 'pools', ...Object.fromEntries(new URLSearchParams(location.hash.split('?')[1] || '')) });
  }, { language, zfsError, parity, state, elevation });
  await page.goto(base);
  await expect(page.locator('[data-array="media"]')).toBeVisible();
}

for (const { width, height, language } of [
  { width: 1440, height: 1000, language: 'pl' },
  { width: 768, height: 1024, language: 'pl' },
  { width: 390, height: 844, language: 'pl' },
  { width: 390, height: 844, language: 'de' },
]) {
  test(`Elastic lista i detal ${width}px ${language}`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    await openElastic(page, { language });
    const cardBox = await page.locator('[data-array="media"]').boundingBox();
    expect(cardBox.x).toBeGreaterThanOrEqual(0);
    expect(cardBox.x + cardBox.width).toBeLessThanOrEqual(width);
    for (const action of ['import', 'create']) {
      const actionBox = await page.locator(`#nas-tab-body > div > .stack > .section-card-head [data-act="${action}"]`).boundingBox();
      expect(actionBox.x).toBeGreaterThanOrEqual(cardBox.x);
      expect(actionBox.x + actionBox.width).toBeLessThanOrEqual(cardBox.x + cardBox.width);
    }
    await page.screenshot({ path: path.join(artifacts, `elastic-list-${width}-${language}.png`), fullPage: true, animations: 'disabled' });
    await page.locator('[data-array="media"] tf-button').click();
    await expect(page.locator('.nas-elastic-detail')).toBeVisible();
    await expect(page).toHaveURL(/array=media/);
    expect(new URLSearchParams((await page.url()).split('?')[1]).has('pool')).toBe(false);
    await page.screenshot({ path: path.join(artifacts, `elastic-detail-${width}-${language}.png`), fullPage: true, animations: 'disabled' });
    await page.locator('.nas-elastic-detail [data-act="back"]').click();
    await expect(page.locator('[data-array="media"]')).toBeVisible();
  });
}

test('Pule bez badge ZFS zachowują count dwóch Elastic, niepełność i izolację węzła', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  await openElastic(page);
  await page.evaluate(() => {
    const screen = window.screenUnderTest;
    screen.nodes.forEach((node) => { node.poolsTotal = 0; });
    const nas = screen.nas;
    const arrays = ['elastic-one', 'elastic-two'].map((name) => ({ ...window.fixture.array, name, unionPath: `/mnt/${name}` }));
    window.badgeFixture = { zfsError: false, delayed: false, calls: [] };
    screen.nas = async function(kind, payload) {
      const nodeId = this.nodeId;
      window.badgeFixture.calls.push({ kind, nodeId });
      if (kind === 'tentaNasPoolsListRequest') {
        if (nodeId === 'helios' && window.badgeFixture.zfsError) throw new Error('Nie można odczytać ZFS');
        return { pools: [], freeDisks: [] };
      }
      if (kind === 'tentaNasElasticArraysListRequest') {
        if (nodeId === 'other') return { arrays: [] };
        if (window.badgeFixture.delayed) return new Promise((resolve) => {
          window.releaseBadgeReply = () => resolve({ arrays });
        });
        return { arrays };
      }
      return Reflect.apply(nas, this, [kind, payload]);
    };
    screen.draw();
  });
  const poolsTab = page.locator('#nas-tabs [data-tab-id="pools"]');
  await expect(poolsTab).toBeVisible();
  await expect(poolsTab.locator('.tf-tab-count')).toHaveCount(0);
  await expect(page.locator('#nas-pools-count')).toHaveAttribute('label', '2');
  await expect(page.locator('#nas-pools-list [data-array]')).toHaveCount(2);
  await expect(page.locator('#nas-tabs [data-tab-id="disks"] .tf-tab-count')).toHaveText('6');
  await page.screenshot({ path: path.join(artifacts, 'pools-no-zfs-badge-two-elastic.png'), fullPage: true, animations: 'disabled' });
  await page.evaluate(() => { window.badgeFixture.zfsError = true; window.screenUnderTest.draw(); });
  await expect(page.locator('#nas-pools-count')).toHaveAttribute('label', '2 + ?');
  await expect(page.locator('#nas-pools-errors')).toContainText('Nie można odczytać ZFS');
  await expect(poolsTab.locator('.tf-tab-count')).toHaveCount(0);
  await page.screenshot({ path: path.join(artifacts, 'pools-no-zfs-badge-partial.png'), fullPage: true, animations: 'disabled' });
  await page.evaluate(() => { window.badgeFixture.delayed = true; window.screenUnderTest.draw(); });
  await expect.poll(() => page.evaluate(() => typeof window.releaseBadgeReply)).toBe('function');
  await page.locator('#nas-node-select select').selectOption('other');
  await expect(page.locator('#nas-pools-count')).toHaveAttribute('label', '0');
  await page.evaluate(async () => { window.releaseBadgeReply(); await new Promise((resolve) => requestAnimationFrame(resolve)); });
  await expect(page.locator('#nas-pools-list [data-array]')).toHaveCount(0);
  await expect(page.locator('#nas-pools-errors')).toBeEmpty();
  await expect(page.locator('#nas-pools-count')).toHaveAttribute('label', '0');
  await expect(poolsTab.locator('.tf-tab-count')).toHaveCount(0);
  expect(await page.evaluate(() => window.badgeFixture.calls.some((call) => call.kind === 'tentaNasElasticArraysListRequest' && call.nodeId === 'other'))).toBe(true);
});

for (const [language, label, creating, unknown, width] of [
  ['pl', 'Oczekuje na montowanie', 'W toku', 'Nie zmierzono', 1440],
  ['en', 'Awaiting mount', 'In progress', 'Not measured', 1440],
  ['de', 'Wartet auf Einhängen', 'In Bearbeitung', 'Nicht gemessen', 1440],
  ['es', 'Pendiente de montaje', 'En curso', 'Sin medir', 1440],
  ['fr', 'En attente de montage', 'En cours', 'Non mesuré', 1440],
  ['pl', 'Oczekuje na montowanie', 'W toku', 'Nie zmierzono', 390],
  ['de', 'Wartet auf Einhängen', 'In Bearbeitung', 'Nicht gemessen', 390],
  ['fr', 'En attente de montage', 'En cours', 'Non mesuré', 390],
]) test(`pending bez joba: lista i N11 ${language} ${width}px`, async ({ page }) => {
  await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 });
  await openElastic(page, { language, state: 'pending' });
  const cardChip = page.locator('[data-array="media"] tf-chip[dot]');
  await expect(cardChip).toHaveAttribute('label', label);
  await expect(cardChip).toHaveAttribute('status', 'warn');
  await expect(page.locator('#nas-tabs [data-tab-id="jobs"] .tf-tab-count')).toHaveCount(0);
  await page.screenshot({ path: path.join(artifacts, `pending-list-${language}-${width}.png`), fullPage: true, animations: 'disabled' });
  await page.locator('[data-array="media"] [data-act="array-details"]').click();
  const detailChip = page.locator('.nas-elastic-detail .grid-2 > .section-card:first-child .section-card-head tf-chip');
  await expect(detailChip).toHaveAttribute('label', label);
  await detailChip.scrollIntoViewIfNeeded();
  await expect(detailChip).toBeVisible();
  const geometry = await detailChip.evaluate((element) => {
    const chip = element.querySelector('.tf-chip');
    const box = chip.getBoundingClientRect();
    const section = element.closest('.section-card').getBoundingClientRect();
    return { left: box.left, right: box.right, sectionLeft: section.left, sectionRight: section.right,
      viewport: innerWidth, clipped: chip.scrollWidth > chip.clientWidth };
  });
  expect(geometry.left).toBeGreaterThanOrEqual(geometry.sectionLeft);
  expect(geometry.right).toBeLessThanOrEqual(Math.min(geometry.sectionRight, width));
  expect(geometry.clipped).toBe(false);
  const restore = page.locator('.nas-elastic-detail [data-act="restore"]');
  await restore.scrollIntoViewIfNeeded();
  await expect(restore).toBeInViewport();
  await page.screenshot({ path: path.join(artifacts, `pending-detail-${language}-${width}.png`), fullPage: true, animations: 'disabled' });
  expect(await page.evaluate(() => window.fixture.jobs)).toEqual([]);
  expect(await page.evaluate(() => window.calls.filter((call) => /ElasticArray(Create|Restore)Request$/.test(call.kind)))).toEqual([]);
  for (const [state, expectedLabel] of [['creating', creating], ['unknown', unknown]]) {
    await page.evaluate((state) => { window.fixture.array.state = state; }, state);
    await page.locator('.nas-elastic-detail [data-act="refresh"]').click();
    await expect(detailChip).toHaveAttribute('label', expectedLabel);
    await expect(detailChip).not.toHaveAttribute('label', label);
    if (state === 'creating') await expect(page.locator('.nas-elastic-detail [data-act="restore"]')).toHaveCount(0);
  }
});

for (const width of [1440, 390]) test(`referencja N11 dla etykiety pending ${width}px`, async ({ page }) => {
  await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 });
  await page.route('**/favicon.ico', (route) => route.fulfill({ status: 204, body: '' }));
  await page.goto(`${base}/mockups/tentanas/n11-pula-unraid.html`);
  await page.evaluate(() => document.fonts.ready);
  await page.evaluate(async () => { await Promise.all(document.getAnimations().filter((animation) => animation.effect.getTiming().iterations !== Infinity).map((animation) => animation.finished)); });
  await expect(page.locator('.d-badges')).toContainText('Elastic Array: aktywna');
  await page.screenshot({ path: path.join(artifacts, `pending-reference-n11-${width}.png`), fullPage: true, animations: 'disabled' });
});

test('Elastic pozostaje dostępne po błędzie ZFS', async ({ page }) => {
  await openElastic(page, { zfsError: true });
  await expect(page.locator('#nas-pools-errors')).toContainText('Nie można odczytać ZFS');
  await page.locator('[data-array="media"] tf-button').click();
  await expect(page.locator('.nas-elastic-detail')).toContainText('/mnt/media');
});

test('mieszana lista nie przewija poziomo przez zamknięte menu ZFS; menu otwiera się i zamyka', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openElastic(page, { language: 'de' });
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(390);
  const trigger = page.locator('[data-pool="tank"] [data-act="more"]');
  await expect.poll(() => page.locator('#nas-tabs').evaluate((tabs) => {
    const active = tabs.querySelector('.tf-tab.active').getBoundingClientRect();
    const scroller = tabs.querySelector('[role="tablist"]').getBoundingClientRect();
    return active.left >= scroller.left && active.right <= scroller.right - 23;
  })).toBe(true);
  await trigger.scrollIntoViewIfNeeded();
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  await trigger.click();
  const menu = page.locator('[data-pool="tank"] tf-menu');
  await expect(menu).toHaveAttribute('open', '');
  const box = await menu.locator('.tf-menu').boundingBox();
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(390);
  await page.locator('#nas-pools-count').click();
  await expect(menu).not.toHaveAttribute('open', '');
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(390);
  await trigger.locator('button').focus();
  await page.keyboard.press('Enter');
  await expect(menu).toHaveAttribute('open', '');
  await page.keyboard.press('Escape');
  await expect(menu).not.toHaveAttribute('open', '');
  await expect(trigger.locator('button')).toBeFocused();
  await page.locator('[data-array="media"] .pc-name').click();
  await expect(page.locator('.nas-elastic-detail')).toBeVisible();
});

test('deep-link po reload zachowuje Elastic, a dostępny przycisk dysku otwiera prawdziwy detal', async ({ page }) => {
  await openElastic(page);
  await page.locator('[data-array="media"] tf-button').click();
  await page.reload();
  await expect(page.locator('.nas-elastic-detail')).toBeVisible();
  await expect(page.locator('.nas-elastic-detail .nas-crumbs')).toContainText('media');
  await expect(page.locator('[data-act="disk"] svg use').first()).toHaveAttribute('href', '#i-external-link');
  await page.getByRole('button', { name: 'Szczegóły dysku d1', exact: true }).click();
  await expect(page).toHaveURL(/disk=vdf/);
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasDiskGetRequest').map((call) => call.payload.diskId))).toEqual(['vdf']);
  await expect(page.locator('.nas-elastic-detail')).toHaveCount(0);
});

for (const outcome of ['job', 'approval', 'unknown']) {
  test(`Restore: ${outcome} ma właściwy wynik bez automatycznego ponowienia`, async ({ page }) => {
    await openElastic(page, { state: 'needs_attention' });
    await page.evaluate((value) => { window.fixture.outcome = value; }, outcome);
    await page.locator('[data-array="media"] tf-button').click();
    await page.locator('[data-act="restore"]').click();
    if (outcome === 'job') {
      await expect(page.locator('#nas-joblog')).toHaveText('Gotowe');
      await page.locator('tf-window [data-action="cancel"]').click();
      await expect(page.locator('#nas-joblog')).toHaveCount(0);
      await expect(page.locator('.nas-elastic-detail [role="status"]')).toContainText('Zadanie');
    } else await expect(page.locator('.nas-elastic-detail [role="status"]')).toContainText(outcome === 'approval' ? 'administratora' : 'nieznany');
    await expect(page.locator('[data-act="restore"]')).toHaveAttribute('disabled', '');
    expect(await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasElasticArrayRestoreRequest').length)).toBe(1);
    if (outcome === 'job') {
      await page.locator('.nas-elastic-detail [data-act="jobs"]').click();
      await expect(page.locator('#nas-jobs-running [data-job="job-elastic"]')).toBeVisible();
      await expect(page.locator('#nas-jobs-running [data-act="cancel"]')).toHaveCount(0);
      await page.locator('#nas-jobs-running [data-act="log"]').click();
      await expect(page.locator('#nas-joblog')).toHaveText('Gotowe');
    }
  });
}

test('pending z obecnymi dyskami odtwarza montowania raz i odczytuje active po jobie', async ({ page }) => {
  await openElastic(page, { state: 'pending' });
  await page.evaluate(() => {
    window.fixture.completeRestore = true;
    for (const disk of [...window.fixture.array.dataDisks, ...window.fixture.array.parityDisks]) {
      disk.mounted = false;
      disk.devicePresent = true;
    }
  });
  await page.locator('[data-array="media"] tf-button').click();
  await page.locator('[data-act="restore"]').click();
  await expect(page.locator('#nas-joblog')).toHaveText('Gotowe');
  await expect(page.locator('.nas-elastic-detail [data-act="restore"]')).toHaveCount(0);
  await expect(page.locator('.nas-elastic-detail')).toContainText('Aktywna');
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasElasticArrayRestoreRequest').length)).toBe(1);
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasElasticArrayGetRequest').length)).toBeGreaterThan(1);
});

test('Restore używa prawdziwego sudo; zmiana węzła podczas remember nie wysyła mutacji', async ({ page }) => {
  await openElastic(page, { state: 'needs_attention', elevation: 'interactive' });
  await page.locator('[data-array="media"] tf-button').click();
  await page.locator('[data-act="restore"]').click();
  await page.locator('#nas-sudo-pass input').fill('test-only');
  await page.locator('#nas-sudo-remember').click();
  await page.locator('tf-window [data-action="confirm"]').click();
  await expect.poll(() => page.evaluate(() => typeof window.finishArm)).toBe('function');
  await page.locator('#nas-node-select select').selectOption('other');
  await page.evaluate(() => window.finishArm({}));
  await expect(page.locator('tf-window')).toHaveCount(0);
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasElasticArrayRestoreRequest').length)).toBe(0);
});

for (const action of ['sync', 'scrub']) for (const outcome of ['job', 'approval', 'unknown']) {
  test(`SnapRAID ${action}: ${outcome}, pojedyncze żądanie i prawdziwy panel zadań`, async ({ page }) => {
    await openElastic(page);
    await page.evaluate((outcome) => { window.fixture.outcome = outcome; }, outcome);
    await page.locator('[data-array="media"] tf-button').click();
    await page.locator(`.nas-snapraid [data-act="${action}"]`).click();
    if (outcome === 'job') {
      await expect(page.locator('#nas-joblog')).toHaveText('Gotowe');
      await page.locator('tf-window [data-action="cancel"]').click();
      await expect(page.locator('tf-window')).toHaveCount(0);
      await expect(page.locator('.nas-snapraid-history tf-chip')).toHaveAttribute('label', 'Sukces');
      await expect(page.locator('.nas-snapraid [data-act="sync"]')).not.toHaveAttribute('disabled', '');
      await expect(page.locator('.kpi tf-stat-card').nth(1)).toHaveAttribute('value', 'Nie zmierzono');
      await page.locator('[data-act="history-job"]').click();
      await expect(page.locator('#nas-joblog')).toHaveText('Gotowe');
      await page.locator('tf-window [data-action="cancel"]').click();
      await expect(page.locator('tf-window')).toHaveCount(0);
      await page.locator('.nas-elastic-detail [data-act="jobs"]').click();
      await expect(page.locator(`#nas-jobs-running [data-job="job-${action}"]`)).toBeVisible();
      await expect(page.locator(`#nas-jobs-running [data-job="job-${action}"] [data-act="cancel"]`)).toHaveCount(0);
    } else {
      await expect(page.locator('.nas-elastic-detail [role="status"]')).toContainText(outcome === 'approval' ? 'drugiego administratora' : 'nieznany');
      await expect(page.locator('tf-window')).toHaveCount(0);
      await page.locator('.nas-elastic-detail [data-act="refresh"]').click();
      for (const kind of ['sync', 'scrub']) await expect(page.locator(`.nas-snapraid [data-act="${kind}"]`)).toHaveAttribute('disabled', '');
    }
    expect(await page.evaluate(() => window.calls.filter((call) => /ElasticArray(Sync|Scrub)Request$/.test(call.kind)).map((call) => ({ kind: call.kind, payload: call.payload })))).toEqual([{ kind: `tentaNasElasticArray${action === 'sync' ? 'Sync' : 'Scrub'}Request`, payload: { name: 'media', sudoPassword: undefined } }]);
  });
}

test('SnapRAID scrub refused nie udaje pomiaru i pozwala na osobny jawny Sync', async ({ page }) => {
  await openElastic(page);
  await page.evaluate(() => { window.fixture.maintenanceOutcome = 'refused'; window.fixture.jobStatus = 'failed'; });
  await page.locator('[data-array="media"] tf-button').click();
  await page.locator('.nas-snapraid [data-act="scrub"]').click();
  await expect(page.locator('#nas-joblog')).toHaveText('Gotowe');
  await page.locator('tf-window [data-action="cancel"]').click();
  await expect(page.locator('tf-window')).toHaveCount(0);
  await expect(page.locator('.nas-snapraid-history tf-chip')).toHaveAttribute('label', 'Nie wykonano');
  await page.locator('.nas-snapraid-history summary').click();
  await expect(page.locator('.nas-snapraid-history details')).toHaveAttribute('open', '');
  await expect(page.locator('.nas-snapraid-history')).toContainText('Wykryto zmiany poza checkpointem');
  await expect(page.locator('.nas-snapraid-history')).toContainText('— / — / —');
  await expect(page.locator('.nas-snapraid > .stat-rows')).toContainText('Ostatni zakończony scrub—');
  await expect(page.locator('.nas-snapraid [data-act="sync"]')).not.toHaveAttribute('disabled', '');
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind.endsWith('SyncRequest')))).toEqual([]);
});

test('SnapRAID prawdziwe sudo i remember nie wysyłają na nowy węzeł', async ({ page }) => {
  await openElastic(page, { elevation: 'interactive' });
  await page.locator('[data-array="media"] tf-button').click();
  await page.locator('.nas-snapraid [data-act="sync"]').click();
  await page.locator('#nas-sudo-pass input').fill('test-only');
  await page.locator('#nas-sudo-remember').click();
  await page.locator('tf-window [data-action="confirm"]').click();
  await expect.poll(() => page.evaluate(() => typeof window.finishArm)).toBe('function');
  await page.locator('#nas-node-select select').selectOption('other');
  await page.evaluate(() => window.finishArm({}));
  await expect(page.locator('tf-window')).toHaveCount(0);
  expect(await page.evaluate(() => window.calls.filter((call) => /ElasticArray(Sync|Scrub)Request$/.test(call.kind)))).toEqual([]);
});

test('SnapRAID prawdziwe sudo przekazuje hasło raz i blokuje akcje przy running', async ({ page }) => {
  await openElastic(page, { elevation: 'interactive' });
  await page.evaluate(() => { window.fixture.jobStatus = 'running'; });
  await page.locator('[data-array="media"] tf-button').click();
  await page.locator('.nas-snapraid [data-act="scrub"]').click();
  await page.locator('#nas-sudo-pass input').fill('test-only');
  await page.locator('tf-window [data-action="confirm"]').click();
  await expect(page.locator('#nas-joblog')).toHaveText('Gotowe');
  await expect(page.locator('#nas-sudo-pass')).toHaveCount(0);
  await page.locator('tf-window [data-action="cancel"]').click();
  await expect(page.locator('tf-window')).toHaveCount(0);
  for (const kind of ['sync', 'scrub']) await expect(page.locator(`.nas-snapraid [data-act="${kind}"]`)).toHaveAttribute('disabled', '');
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind.endsWith('ScrubRequest')).map((call) => call.payload))).toEqual([{ name: 'media', sudoPassword: 'test-only' }]);
});

for (const [width, language] of [[1440, 'pl'], [768, 'en'], [390, 'pl'], [390, 'en'], [390, 'de']]) {
  test(`SnapRAID karta i historia ${language} ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: width === 390 ? 844 : width === 768 ? 1024 : 1000 });
    await openElastic(page, { language });
    await page.evaluate(() => {
      const success = { kind: 'scrub', outcome: 'ok', jobId: 'job-scrub', operationId: 'op-scrub', startedAt: '2026-09-08T12:00:00Z', finishedAt: '2026-09-08T12:01:00Z', exitCode: 0, totalBlocks: 100, checkedBlocks: 68, errorsFile: 0, errorsIo: 0, errorsData: 0 };
      window.fixture.array.snapraid.lastScrub = success;
      window.fixture.array.snapraid.history = [{ ...success, outcome: 'refused', jobId: 'job-refused', finishedAt: '2026-09-08T12:02:00Z', exitCode: null, totalBlocks: null, checkedBlocks: null, errorsFile: null, errorsIo: null, errorsData: null, detail: 'unsynced_changes' }, success];
    });
    await page.locator('[data-array="media"] tf-button').click();
    const card = page.locator('.nas-snapraid');
    await expect(card).toBeVisible();
    await card.locator('[data-act="sync"]').scrollIntoViewIfNeeded();
    const box = await card.boundingBox();
    for (const [action, variant] of [['sync', 'secondary'], ['scrub', 'ghost']]) {
      const cta = card.locator(`> .section-card-head [data-act="${action}"]`);
      await expect(cta).toHaveAttribute('variant', variant);
      const rect = await cta.boundingBox();
      expect(rect.x).toBeGreaterThanOrEqual(box.x);
      expect(rect.x + rect.width).toBeLessThanOrEqual(box.x + box.width);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width);
    await page.screenshot({ path: path.join(artifacts, `snapraid-${language}-${width}.png`), fullPage: true, animations: 'disabled' });
    await card.screenshot({ path: path.join(artifacts, `snapraid-card-${language}-${width}.png`), animations: 'disabled' });
    await expect(card.locator('.nas-snapraid-history li')).toHaveCount(2);
    expect(await page.evaluate(() => window.calls.filter((call) => /ElasticArray(Sync|Scrub)Request$/.test(call.kind)))).toEqual([]);
  });
}

test('SnapRAID 20 prób pozostaje kompaktowe, a otwarte pomiary przetrwają odświeżenie', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openElastic(page, { language: 'en' });
  await page.evaluate(() => {
    window.fixture.array.snapraid.history = Array.from({ length: 20 }, (_, i) => ({ operationId: `operation-${i}`, jobId: `job-${i}`, kind: i % 2 ? 'sync' : 'scrub', outcome: 'refused', detail: 'unsynced_changes', startedAt: '2026-09-08T12:00:00Z', finishedAt: '2026-09-08T12:01:00Z', errorsFile: null, errorsIo: null, errorsData: null }));
  });
  await page.locator('[data-array="media"] tf-button').click();
  const history = page.locator('.nas-snapraid-history');
  await expect(history.locator('li')).toHaveCount(20);
  await expect(history.locator('details[open]')).toHaveCount(0);
  expect((await history.boundingBox()).height).toBeLessThan(2200);
  await history.screenshot({ path: path.join(artifacts, 'snapraid-history20-collapsed-en390.png'), animations: 'disabled' });
  const last = history.locator('details').last();
  await last.locator('summary').click();
  await expect(last).toHaveAttribute('open', '');
  await expect(last.locator('.stat-rows')).toBeVisible();
  await expect(last).toContainText('Changes exist outside the checkpoint');
  await page.locator('.nas-elastic-detail [data-act="refresh"]').click();
  await expect(history.locator('details[open]')).toHaveCount(1);
  await expect(last).toHaveAttribute('open', '');
  await last.scrollIntoViewIfNeeded();
  await last.screenshot({ path: path.join(artifacts, 'snapraid-history20-expanded-en390.png'), animations: 'disabled' });
  expect(await page.evaluate(() => window.calls.filter((call) => /ElasticArray(Sync|Scrub)Request$/.test(call.kind)))).toEqual([]);
});

test('SnapRAID nieukończona maintenance ukrywa Restore, ale Refused nie blokuje legalnego pending', async ({ page }) => {
  await openElastic(page, { state: 'pending' });
  await page.locator('[data-array="media"] tf-button').click();
  for (const outcome of ['running', 'failed', 'needs_attention']) {
    await page.evaluate((outcome) => { window.fixture.array.snapraid.history = [{ kind: 'sync', outcome }]; }, outcome);
    await page.locator('.nas-elastic-detail [data-act="refresh"]').click();
    await expect(page.locator('.nas-elastic-detail [data-act="restore"]')).toHaveCount(0);
  }
  await page.evaluate(() => { window.fixture.array.snapraid.history = [{ kind: 'scrub', outcome: 'refused', detail: 'unsynced_changes' }]; window.fixture.completeRestore = true; });
  await page.locator('.nas-elastic-detail [data-act="refresh"]').click();
  await page.locator('.nas-elastic-detail [data-act="restore"]').click();
  await expect(page.locator('#nas-joblog')).toHaveText('Gotowe');
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind.endsWith('RestoreRequest')).length)).toBe(1);
  expect(await page.evaluate(() => window.calls.filter((call) => /ElasticArray(Sync|Scrub)Request$/.test(call.kind)))).toEqual([]);
});

async function configureElastic(page, { filesystem = 'xfs', parity = [] } = {}) {
  await page.locator('#nas-tab-body tf-button[data-act="create"]').click();
  await page.locator('#nas-pw-kind tf-choice-card[value="elastic"]').click();
  await page.locator('[data-wizard-next]').click();
  await page.locator('#nas-pw-disks [data-disk="vdb"] tf-checkbox').click();
  await page.locator(`#nas-pw-filesystem .tf-seg-opt[data-value="${filesystem}"]`).click();
  await page.locator('[data-wizard-next]').click();
  for (const diskId of parity) await page.locator(`#nas-pw-parity [data-disk="${diskId}"] tf-checkbox`).click();
  await page.locator('#nas-pw-name input').fill('archive');
  await page.locator('[data-pw-preview]').click();
  await expect(page.locator('#nas-pw-preview')).toContainText('/mnt/archive');
  await page.locator('[data-wizard-next]').click();
  await page.locator('#nas-pw-confirm input').fill('archive');
}

for (const { filesystem, parity } of [
  { filesystem: 'xfs', parity: ['vdd', 'vde'] },
  { filesystem: 'ext4', parity: [] },
  { filesystem: 'ext4', parity: ['vdd'] },
  { filesystem: 'ext4', parity: ['vdd', 'vde'] },
]) {
  test(`Create ${filesystem} z ${parity.length} parity przekazuje dokładny podgląd i potwierdzenie`, async ({ page }) => {
    await openElastic(page);
    await configureElastic(page, { filesystem, parity });
    await page.locator('[data-wizard-next]').click();
    await expect(page.locator('.result-box.ok')).toBeVisible();
    const requests = await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasElasticArrayCreateRequest' || call.kind === 'tentaNasElasticArrayPlanRequest'));
    expect(requests).toHaveLength(2);
    expect(requests[0].payload).toMatchObject({ name: 'archive', filesystem, dataDiskIds: ['vdb'], parityDiskIds: parity });
    expect(requests[1].payload).toMatchObject({ name: 'archive', filesystem, dataDiskIds: ['vdb'], parityDiskIds: parity, confirmName: 'archive' });
  });
}

for (const outcome of ['approval', 'unknown']) {
  test(`Create bez parity: ${outcome} nie udaje utworzonej macierzy`, async ({ page }) => {
    await openElastic(page);
    await page.evaluate((value) => { window.fixture.outcome = value; }, outcome);
    await configureElastic(page);
    await expect(page.locator('.install-step-body')).toContainText('Bez parity');
    await page.locator('[data-wizard-next]').click();
    await expect(page.locator('.install-step-body')).toContainText(outcome === 'approval' ? 'administratora' : 'nieznany');
    await expect(page.locator('.result-box.ok')).toHaveCount(0);
    expect(await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasJobGetRequest').length)).toBe(0);
    await page.locator('[data-wizard-next]').click();
    await expect(page.locator('tf-window')).toHaveCount(0);
    await expect(page).toHaveURL(outcome === 'approval' ? /tab=jobs/ : /tab=pools/);
    const requests = await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasElasticArrayCreateRequest'));
    expect(requests).toHaveLength(1);
    expect(requests[0].payload.parityDiskIds).toEqual([]);
  });
}

test('Create blokuje potwierdzenie podczas prawdziwego sudo i nie wysyła po zmianie węzła', async ({ page }) => {
  await openElastic(page, { elevation: 'interactive' });
  await configureElastic(page);
  await page.locator('[data-wizard-next]').click();
  await expect(page.locator('#nas-pw-confirm input')).toBeDisabled();
  await page.locator('#nas-sudo-pass input').fill('test-only');
  await page.locator('#nas-sudo-remember').click();
  await page.locator('tf-window [data-action="confirm"]').click();
  await expect.poll(() => page.evaluate(() => typeof window.finishArm)).toBe('function');
  await page.locator('#nas-node-select select').selectOption('other');
  await page.evaluate(() => window.finishArm({}));
  await expect(page.locator('#nas-sudo-pass')).toHaveCount(0);
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasElasticArrayCreateRequest').length)).toBe(0);
});

test('Create z prawdziwym sudo remember wysyła dokładnie jedną zaakceptowaną konfigurację', async ({ page }) => {
  await openElastic(page, { elevation: 'interactive' });
  await configureElastic(page);
  await page.locator('[data-wizard-next]').click();
  await expect(page.locator('#nas-pw-confirm input')).toBeDisabled();
  await page.locator('#nas-sudo-pass input').fill('test-only');
  await page.locator('#nas-sudo-remember').click();
  await page.locator('tf-window [data-action="confirm"]').click();
  await expect.poll(() => page.evaluate(() => typeof window.finishArm)).toBe('function');
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasElasticArrayCreateRequest').length)).toBe(0);
  await page.evaluate(() => window.finishArm({}));
  await expect(page.locator('.result-box.ok')).toBeVisible();
  const requests = await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasElasticArrayCreateRequest'));
  expect(requests).toHaveLength(1);
  expect(requests[0].payload).toEqual({ name: 'archive', filesystem: 'xfs', dataDiskIds: ['vdb'], parityDiskIds: [], confirmName: 'archive', sudoPassword: undefined });
});

for (const { width, height, language } of [
  { width: 1440, height: 1000, language: 'pl' },
  { width: 768, height: 1024, language: 'pl' },
  { width: 390, height: 844, language: 'pl' },
  { width: 390, height: 844, language: 'de' },
]) {
  test(`Elastic kreator ${width}px ${language}`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    await openElastic(page, { language });
    await page.locator('#nas-tab-body tf-button[data-act="create"]').click();
    const win = page.locator('tf-window.nas-elastic-wizard');
    await page.locator('#nas-pw-kind tf-choice-card[value="elastic"]').click();
    await page.screenshot({ path: path.join(artifacts, `elastic-wizard-kind-${width}-${language}.png`), animations: 'disabled' });
    await page.locator('[data-wizard-next]').click();
    await page.locator('#nas-pw-disks [data-disk="vdb"] tf-checkbox').click();
    await expect(page.locator('#nas-pw-filesystem')).toBeVisible();
    await page.screenshot({ path: path.join(artifacts, `elastic-wizard-data-${width}-${language}.png`), animations: 'disabled' });
    await page.locator('[data-wizard-next]').click();
    await page.locator('#nas-pw-parity [data-disk="vdd"] tf-checkbox').click();
    await page.locator('#nas-pw-name input').fill('archive');
    await expect(page.locator('[data-wizard-next]')).toHaveAttribute('disabled', '');
    await page.locator('[data-pw-preview]').click();
    await expect(page.locator('#nas-pw-preview')).toContainText('/mnt/archive');
    await page.screenshot({ path: path.join(artifacts, `elastic-wizard-parity-${width}-${language}.png`), animations: 'disabled' });
    await page.locator('[data-wizard-next]').click();
    await expect(page.locator('#nas-pw-summary')).toContainText('archive');
    const summaryGeometry = await page.locator('#nas-pw-summary').evaluate((host) => [...host.shadowRoot.querySelectorAll('tbody td')].map((cell) => {
      const box = cell.getBoundingClientRect();
      const range = document.createRange();
      range.selectNodeContents(cell);
      return { text: cell.textContent.trim(), left: box.left, right: box.right, fragments: [...range.getClientRects()].map((r) => ({ left: r.left, right: r.right })) };
    }));
    expect(summaryGeometry.some((cell) => cell.text.includes('32 GiB') && cell.text.includes('ELASTIC-vdb') && cell.text.includes('xfs'))).toBe(true);
    for (const cell of summaryGeometry) for (const fragment of cell.fragments) {
      expect(fragment.left).toBeGreaterThanOrEqual(cell.left - 1);
      expect(fragment.right).toBeLessThanOrEqual(cell.right + 1);
    }
    await page.locator('#nas-pw-confirm input').fill('archive');
    const geometry = await win.evaluate((host) => {
      const frame = host.shadowRoot.querySelector('.tf-window');
      const rect = frame.getBoundingClientRect();
      const overflow = [...document.querySelectorAll('#nas-root *')].filter((element) => element.getBoundingClientRect().right > innerWidth + 1).map((element) => ({ tag: element.tagName, className: element.className?.baseVal ?? element.className, right: element.getBoundingClientRect().right })).slice(-15);
      return { widthAttribute: host.getAttribute('width'), widthInline: frame.style.width, minWidth: frame.style.minWidth, maxWidth: getComputedStyle(frame).maxWidth, frame: { x: rect.x, width: rect.width }, hostWidth: host.getBoundingClientRect().width, viewport: innerWidth, scrollWidth: document.documentElement.scrollWidth, overflow };
    });
    await test.info().attach('geometria-kreatora', { body: JSON.stringify(geometry), contentType: 'application/json' });
    console.log('geometria-kreatora', geometry);
    await page.screenshot({ path: path.join(artifacts, `elastic-wizard-summary-${width}-${language}.png`), animations: 'disabled' });
    if (width === 390) {
      const scrollBody = win.locator('.tf-window-body');
      await scrollBody.evaluate((element) => { element.scrollTop = 0; });
      await page.screenshot({ path: path.join(artifacts, `elastic-wizard-summary-top-${width}-${language}.png`), animations: 'disabled' });
      await scrollBody.evaluate((element) => { element.scrollTop = element.scrollHeight; });
      const inputBox = await page.locator('#nas-pw-confirm input').boundingBox();
      const footerBox = await win.locator('.tf-window-footer').boundingBox();
      expect(inputBox.y + inputBox.height).toBeLessThanOrEqual(footerBox.y + 1);
      await page.screenshot({ path: path.join(artifacts, `elastic-wizard-summary-bottom-${width}-${language}.png`), animations: 'disabled' });
    }
    const afterBox = await win.locator('.tf-window').boundingBox();
    expect(afterBox.width).toBeCloseTo(geometry.frame.width, 0);
    await page.locator('[data-wizard-next]').click();
    await expect(page.locator('.result-box.ok')).toBeVisible();
    const create = await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasElasticArrayCreateRequest'));
    expect(create).toHaveLength(1);
    expect(create[0].payload).toMatchObject({ name: 'archive', dataDiskIds: ['vdb'], parityDiskIds: ['vdd'], filesystem: 'xfs', confirmName: 'archive' });
    await page.locator('[data-wizard-next]').click();
    await expect(win).toHaveCount(0);
    await expect(page).toHaveURL(/array=archive/);
  });
}
