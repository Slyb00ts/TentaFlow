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
        if (kind === 'tentaNasJobGetRequest') return { job: { ...window.fixture.jobs[0], status: window.fixture.jobStatus || 'succeeded', progressPct: 100, log: ['Gotowe'] } };
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
