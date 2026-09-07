// =============================================================================
// Plik: tentanas-targets.spec.js
// Opis: Testy przeglądarkowe detalu N19 i ponownego wyboru portalu w kreatorze.
//       Moduły, komponenty, style i tłumaczenia są rzeczywiste; transport jest fixture.
// =============================================================================

const { test, expect } = require('@playwright/test');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');

const root = path.resolve(__dirname, '../../tentaflow-core/www');
const design = path.resolve(process.env.TENTANAS_MOCKUP_ROOT || path.resolve(__dirname, '../../../new_apps'));
const artifacts = path.resolve(process.env.TENTANAS_E2E_ARTIFACTS || path.join(design, 'reviews/artifacts/T02'));
let server;
let base;
const browserErrors = new WeakMap();

async function footerGeometry(footer) {
  return footer.evaluate((element) => {
    const rect = (node) => {
      const { x, y, width, height } = node.getBoundingClientRect();
      return { x, y, width, height };
    };
    const style = getComputedStyle(element);
    return {
      box: rect(element), display: style.display, gap: style.gap,
      justifyContent: style.justifyContent, flexWrap: style.flexWrap,
      buttons: [...element.children].filter((child) => child.matches('tf-button, button')).map((button) => ({
        text: button.textContent.trim(), ...rect(button),
      })),
    };
  });
}

test.use({ viewport: { width: 1440, height: 1080 } });

test.beforeAll(async () => {
  await fs.mkdir(artifacts, { recursive: true });
  const index = await fs.readFile(path.join(root, 'index.html'), 'utf8');
  const sprite = index.match(/<svg[^>]*(?:data-role="sprite"|aria-hidden="true")[\s\S]*?<\/svg>/)?.[0];
  expect(sprite).toContain('id="i-target"');
  const html = `<!doctype html><html lang="pl"><head><meta charset="utf-8"><link rel="icon" href="data:,">
    ${['controls', 'style', 'compat', 'install-wizard', 'tentanas'].map((name) => `<link rel="stylesheet" href="/css/${name}.css">`).join('')}
    </head><body>${sprite}<main id="nas-root" class="nas-root" style="padding:24px"><div id="nas-tab-body"></div></main></body></html>`;
  server = http.createServer(async (req, res) => {
    const pathname = new URL(req.url, 'http://localhost').pathname;
    if (pathname === '/favicon.ico') { res.writeHead(204).end(); return; }
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
  const errors = [];
  browserErrors.set(page, errors);
  page.on('pageerror', (error) => errors.push(error.message));
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
});

test.afterEach(async ({ page }, testInfo) => {
  await testInfo.attach('konsola', { body: JSON.stringify(browserErrors.get(page)), contentType: 'application/json' });
  expect(browserErrors.get(page)).toEqual([]);
});

for (const { width, language } of [
  { width: 1440, language: 'pl' },
  { width: 390, language: 'pl' },
  { width: 390, language: 'de' },
]) {
  test(`N14 stopka i akcje ${width}px ${language}`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1080 });
    await openDetail(page);
    await page.locator('[data-act="back"]').click();
    await page.evaluate(async (language) => {
      const { I18n } = await import('/js/i18n.js');
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      const action = ApiBinary.action;
      ApiBinary.action = (kind, payload, ...rest) => kind === 'mePreferencesUpdateRequest'
        ? Promise.resolve({ language: payload.language }) : action(kind, payload, ...rest);
      await I18n.setLanguage(language);
      const screen = window.screenUnderTest;
      const receive = screen.nas;
      screen.nas = async (kind, payload) => {
        if (kind !== 'tentaNasTargetCreateRequest') return receive(kind, payload);
        window.calls.push({ kind, payload });
        return { job: { jobId: 'job-create-footer', kind: 'target-create' } };
      };
    }, language);
    const measurements = [];
    const snapshot = async (step) => {
      const footer = page.locator('tf-window.nas-modal [slot="footer"]');
      await expect(footer).toBeVisible();
      const dialog = page.locator('tf-window.nas-modal .tf-window');
      await expect.poll(() => dialog.evaluate((element) => element.getAnimations().every((animation) => animation.playState === 'finished'))).toBe(true);
      const geometry = await footerGeometry(footer);
      const dialogBox = await dialog.boundingBox();
      expect(dialogBox.width).toBe(width === 1440 ? 822 : 376);
      measurements.push({ step, dialog: dialogBox, ...geometry });
      expect(geometry.buttons).toHaveLength(3);
      expect(geometry.display).toBe('flex');
      expect(geometry.gap).toBe('8px');
      expect(geometry.justifyContent).toBe('flex-end');
      if (language === 'pl') {
        expect(new Set(geometry.buttons.map((button) => button.y)).size).toBe(1);
      }
      for (let i = 1; i < geometry.buttons.length; i++) {
        const previous = geometry.buttons[i - 1];
        const current = geometry.buttons[i];
        if (current.y === previous.y) expect(current.x - previous.x - previous.width).toBeCloseTo(8, 1);
      }
      for (const button of geometry.buttons) {
        expect(button.x).toBeGreaterThanOrEqual(0);
        expect(button.x + button.width).toBeLessThanOrEqual(width);
        expect(button.y + button.height).toBeLessThanOrEqual(1080);
      }
      await footer.screenshot({ path: path.join(artifacts, `n14-${width}-${language}-step${step}-footer.png`), animations: 'disabled' });
      await page.screenshot({ path: path.join(artifacts, `n14-${width}-${language}-step${step}.png`), animations: 'disabled' });
    };
    const prepare = async (record) => {
      await page.locator('[data-act="create-target"]').click();
      await expect(page.locator('[data-wizard-next]')).toHaveAttribute('disabled', '');
      if (record) await snapshot(1);
      await page.locator('#nas-tw-name input').fill('footer-test');
      await page.locator('[data-wizard-next]').click();
      await page.locator('[data-wizard-back]').click();
      await expect(page.locator('#nas-tw-name input')).toHaveValue('footer-test');
      await page.locator('[data-wizard-next]').click();
      await page.locator('#nas-tw-auth [data-value="none"]').click();
      if (record) await snapshot(2);
      await page.locator('[data-wizard-next]').click();
      if (record) await snapshot(3);
    };
    await prepare(true);
    await fs.writeFile(path.join(artifacts, `n14-${width}-${language}-geometry.json`), JSON.stringify(measurements, null, 2));
    await page.locator('[data-wizard-cancel]').click();
    await expect(page.locator('[data-wizard-next]')).toHaveCount(0);
    expect(await page.evaluate(() => window.calls.filter((call) => call.kind.includes('CreateRequest')))).toEqual([]);
    await prepare(false);
    await page.locator('[data-wizard-next]').click();
    await expect.poll(() => page.evaluate(() => window.jobId)).toBe('job-create-footer');
    const calls = await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasTargetCreateRequest'));
    expect(calls).toHaveLength(1);
    expect(calls[0].payload.name).toBe('footer-test');
    await expect(page.locator('[data-wizard-next]')).toHaveCount(0);
  });
}

test('N14 mockup stopki przy tych samych szerokościach dialogu', async ({ page }) => {
  await page.goto(`${base}/mockups/tentanas/n14-kreator-target.html`);
  const windows = page.locator('.window');
  await expect(windows).toHaveCount(3);
  await page.evaluate(() => document.fonts.ready);
  const measurements = [];
  for (const width of [822, 376]) {
    await windows.evaluateAll((elements, width) => elements.forEach((element) => { element.style.width = `${width}px`; }), width);
    for (let i = 0; i < 3; i++) {
      const footer = windows.nth(i).locator('.window-foot');
      const dialogBox = await windows.nth(i).boundingBox();
      expect(dialogBox.width).toBe(width);
      measurements.push({ width, step: i + 1, dialog: dialogBox, ...await footerGeometry(footer) });
      await footer.screenshot({ path: path.join(artifacts, `n14-mockup-${width}-step${i + 1}-footer.png`) });
    }
  }
  await fs.writeFile(path.join(artifacts, 'n14-mockup-geometry.json'), JSON.stringify(measurements, null, 2));
});

async function openDetail(page, { admin = true, interfaces = null, protocol = 'nvmet', sessionsKnown = false, sessions = 0, deleteFailure = false, detail = 'portal 10.10.0.7 is not on storage1 any more — storage1 now has 10.10.0.9, and the address moved to bond0, which nobody picked for this target — nothing is exported on it, because this target is not in the kernel; the target stays as it is until an admin re-picks the interface', networkFailure = false } = {}) {
  await page.addInitScript(async ({ admin, interfaces, detail, networkFailure, protocol, sessionsKnown, sessions, deleteFailure }) => {
    if (document.readyState === 'loading') await new Promise((resolve) => document.addEventListener('DOMContentLoaded', resolve, { once: true }));
    localStorage.setItem('tentaflow_lang', 'pl');
    const { I18n } = await import('/js/i18n.js');
    await I18n.init();
    const { default: screenModule } = await import('/js/modules/tentanas.js');
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    ApiBinary.one = async (kind) => {
      if (kind === 'authMeRequest') return { role: admin ? 'admin' : 'reader' };
      if (kind === 'tentaNasNodesListRequest') return { localNodeId: 'helios', nodes: ['helios', 'other'].map((nodeId) => ({ nodeId, nodeName: nodeId, isLocal: nodeId === 'helios', instanceStatus: 'ready', disksTotal: 4, poolsTotal: 2, sharesTotal: 1, ramBytes: 137438953472 })) };
      throw new Error(`Nieoczekiwane żądanie powłoki ${kind}`);
    };
    const target = {
      targetId: 'scratch', name: 'scratch', protocol: 'nvmet', enabled: true,
      wwn: 'nqn.2026-09.local.tentaflow:helios.scratch', state: 'error', stateDetail: detail,
      luns: [{ source: 'fast/scratch', sizeBytes: 536870912000, thin: true }],
      portals: ['tcp', 'rdma'].map((transport) => ({ interface: 'storage1', address: '10.10.0.7', port: 4420, transport })),
      auth: { method: 'dhchap', secretSet: true, dhchapHash: 'hmac(sha256)', dhchapDhgroup: 'ffdhe2048' },
      initiators: ['nqn.2014-08.org.nvmexpress:uuid:9f2c-a17b'],
      portGroups: [{ groupId: 1, state: 'optimized' }], sessionsKnown, sessions,
    };
    if (protocol === 'iscsi') {
      Object.assign(target, { targetId: 'vm-store', name: 'vm-store', protocol, state: 'active', stateDetail: '',
        wwn: 'iqn.2026-09.local.tentaflow:helios.vm-store',
        luns: [{ source: 'tank/vm-store', sizeBytes: 2199023255552, thin: true }],
        portals: [{ interface: 'storage0', address: '10.10.0.5', port: 3260, transport: 'tcp' }],
        auth: { method: 'mutual-chap', username: 'tentanas-vmstore', mutualUsername: 'vmhost-01', secretSet: true, mutualSecretSet: true },
        initiators: ['iqn.1994-05.com.redhat:vmhost-01', 'iqn.1994-05.com.redhat:vmhost-02'],
      });
    }
    const caps = { iscsi: true, nvmet: true, nvmeRdma: true, dhchap: true,
      interfaces: interfaces || [
        { name: 'storage1', address: '10.10.0.9', supported: true, rdma: true, shared: false },
        { name: 'bond0', address: '10.10.0.7', supported: true, rdma: true, shared: false },
        { name: 'storage0', address: '10.10.0.5', supported: true, rdma: true, shared: false },
        { name: 'eno1', address: '192.168.1.40', supported: true, rdma: false, shared: true },
      ], volumes: [{ name: 'fast/scratch', sizeBytes: 536870912000, thin: true, exportedBy: 'scratch' }],
    };
    window.calls = [];
    window.fixture = { target, caps, networkFailure, deleteFailure };
    const screen = Object.assign(Object.create(screenModule), {
      root: document.querySelector('main'), nodeId: 'helios', tab: 'shares', timers: new Set(), disposed: false,
      isAdmin: admin, targetName: target.name,
      withSudo: async (fn) => fn(null), openJobLog: (id) => { window.jobId = id; },
      nas: async (kind, payload) => {
        window.calls.push({ kind, payload });
        if (kind === 'tentaNasJobsListRequest') return { jobs: [] };
        if (kind === 'tentaNasEnvironmentRequest') return { environment: { uptimeSecs: 86400, probedAt: '2026-09-07T10:00:00Z', features: [{ id: 'zfs', status: 'ok', version: '2.3.0' }, { id: 'iscsi', status: 'ok' }, { id: 'nvmet', status: 'ok' }], elevation: { mode: 'helper', helperState: 'ok', coreUser: 'tentaflow', coreVersion: 'test' } } };
        if (kind === 'tentaNasSharesListRequest') return { shares: [], users: [] };
        if (kind === 'tentaNasTargetGetRequest') return { target: window.fixture.target, sessions: Array.from({ length: sessions }, (_, i) => ({ client: `10.10.0.${21 + i}`, user: target.initiators[i] })), configPreview: '/sys/kernel/config/\n  secret = ***' };
        if (kind === 'tentaNasTargetsListRequest') {
          if (window.fixture.networkFailure) throw new Error('Brak odczytu interfejsów');
          return { targets: [target, ...(window.fixture.siblings || [])], capabilities: caps };
        }
        if (kind === 'tentaNasTargetUpdateRequest') return { job: { jobId: 'job-portal', kind: 'target-update' } };
        if (kind === 'tentaNasTargetDeleteRequest') {
          if (window.fixture.deleteFailure) throw new Error('Jądro odmówiło usunięcia targetu');
          return { job: { jobId: 'job-delete', kind: 'target-delete' } };
        }
        throw new Error(`Nieoczekiwane żądanie ${kind}`);
      },
    });
    window.screenUnderTest = screen;
    const mountRoute = () => {
      const params = Object.fromEntries(new URLSearchParams(location.hash.split('?')[1] || ''));
      return screen.mount({ node: 'helios', tab: 'shares', ...(networkFailure ? { target: target.targetId } : {}), ...params });
    };
    window.addEventListener('hashchange', mountRoute);
    await mountRoute();
  }, { admin, interfaces, detail, networkFailure, protocol, sessionsKnown, sessions, deleteFailure });
  await page.goto(base);
  await expect(page.getByTestId('portal_configured')).toHaveText(protocol === 'iscsi' ? '10.10.0.5:3260' : '10.10.0.7:4420');
}

test('dryf N19: faktyczny interfejs, jawny wybór i zapis dopiero po podsumowaniu', async ({ page }) => {
  await openDetail(page);
  await expect(page.getByTestId('portal_expected')).toHaveText('storage1');
  await expect(page.getByTestId('portal_actual')).toHaveText('bond0');
  await expect(page.getByTestId('portal_current_addresses')).toHaveText('10.10.0.9');
  await expect(page.getByTestId('portal_exposure')).toHaveText('Nie zmierzono');
  await expect(page.getByTestId('portal-drift-banner')).toContainText('nothing is exported');
  await expect(page.locator('#nas-td-interfaces tr').filter({ hasText: '192.168.1.40' })).toContainText('LAN — współdzielony');
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: path.join(artifacts, 'n19-detail-desktop.png'), animations: 'disabled' });
  const cards = page.locator('.nas-target-card');
  for (let i = 0; i < await cards.count(); i++) await cards.nth(i).screenshot({ path: path.join(artifacts, `n19-card-${i}.png`), animations: 'disabled' });
  await expect(page.locator('.nas-target-detail tf-empty-state')).toContainText('Nie wiadomo — to nie znaczy zero');
  await page.getByTestId('target-portal-card').screenshot({ path: path.join(artifacts, 'n19-detail-content.png'), animations: 'disabled' });
  await page.locator('#nas-td-preview').scrollIntoViewIfNeeded();
  await page.screenshot({ path: path.join(artifacts, 'n19-detail-bottom.png'), animations: 'disabled' });
  await page.locator('[data-act="repick-portal"]').click();
  const picker = page.locator('#nas-tw-iface select');
  await expect(picker).toHaveValue('__tentanas_no_portal__');
  await expect(page.locator('[data-wizard-next]')).toHaveAttribute('disabled', '');
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind.includes('Update')).length)).toBe(0);
  await expect(page.locator('.install-step-body')).toBeVisible();
  await page.screenshot({ path: path.join(artifacts, 'n19-repick-unselected.png'), animations: 'disabled' });
  await picker.selectOption('storage1');
  await page.locator('[data-wizard-next]').click();
  await expect(page.locator('.install-step-body')).toContainText('10.10.0.9');
  await expect(page.locator('.install-step-body .wizard-warning.danger').filter({ hasText: '10.10.0.7' })).toContainText('10.10.0.9');
  await page.screenshot({ path: path.join(artifacts, 'n19-repick-summary.png'), animations: 'disabled' });
  await page.locator('[data-wizard-next]').click();
  await expect.poll(() => page.evaluate(() => window.jobId)).toBe('job-portal');
  const updates = await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasTargetUpdateRequest'));
  expect(updates).toHaveLength(1);
  expect(updates[0].payload.repickPortal).toBe(true);
  expect(updates[0].payload.portals.map((portal) => portal.interface)).toEqual(['storage1', 'storage1']);
});

test('anulowanie i brak domyślnego nie zmieniają eksportu', async ({ page }) => {
  await openDetail(page);
  await page.locator('[data-act="repick-portal"]').click();
  await page.locator('#nas-tw-iface select').selectOption('');
  await expect(page.locator('[data-wizard-next]')).toHaveAttribute('disabled', '');
  await page.locator('[data-wizard-cancel]').click();
  await expect(page.locator('[data-wizard-cancel]')).toHaveCount(0);
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind.includes('Update')))).toEqual([]);
});

test('wybór w tabeli wymaga kreatora; alias nie obiecuje nieobsługiwanego przypięcia', async ({ page }) => {
  await openDetail(page, { interfaces: [
    { name: 'storage1', address: '10.10.0.9', supported: true, rdma: true },
    { name: 'storage1', address: '10.10.0.8', supported: true, rdma: true },
    { name: 'bond0', address: '10.10.0.7', supported: true, rdma: true },
  ] });
  const row = page.locator('#nas-td-interfaces tr').filter({ hasText: '10.10.0.8' });
  await expect(row.locator('tf-button')).toHaveAttribute('disabled', '');
  await page.locator('#nas-td-interfaces tr').filter({ hasText: '10.10.0.9' }).locator('tf-button').click();
  await expect(page.locator('#nas-tw-iface select')).toHaveValue('storage1');
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind.includes('Update')))).toEqual([]);
  await page.locator('[data-wizard-next]').click();
  await expect(page.locator('.install-step-body')).toContainText('10.10.0.9:4420');
  await page.locator('[data-wizard-cancel]').click();
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind.includes('Update')))).toEqual([]);
});

test('alias nie oznacza dryfu, a zwykła edycja zachowuje zapisany adres', async ({ page }) => {
  await openDetail(page, { interfaces: [
    { name: 'storage1', address: '10.10.0.9', supported: true, rdma: true },
    { name: 'storage1', address: '10.10.0.7', supported: true, rdma: true },
  ], detail: '' });
  await expect(page.getByTestId('portal-drift-banner')).toHaveCount(0);
  await page.locator('.nas-target-detail [data-act="edit"]').click();
  await page.locator('[data-wizard-next]').click();
  await expect(page.locator('.install-step-body .stat-rows')).toContainText('10.10.0.7:4420');
  await page.locator('[data-wizard-next]').click();
  await expect.poll(() => page.evaluate(() => window.jobId)).toBe('job-portal');
  expect(await page.evaluate(() => window.calls.find((call) => call.kind.includes('Update')).payload.repickPortal)).toBeUndefined();
});

test('nieudana sonda nie wymyśla dryfu ani faktycznego interfejsu', async ({ page }) => {
  await openDetail(page, { networkFailure: true });
  await expect(page.getByTestId('portal_actual')).toHaveText('Nie zmierzono');
  await expect(page.getByTestId('portal-drift-banner')).toHaveCount(0);
});

test('odświeżenie zmienia obserwację adresu; czytelnik nie dostaje CTA mutacji', async ({ page }) => {
  await openDetail(page, { admin: false, detail: 'the export is reachable there' });
  await expect(page.getByTestId('portal-drift-banner')).toContainText('the export is reachable there');
  await expect(page.locator('[data-act="repick-portal"]')).toHaveCount(0);
  await page.evaluate(() => { window.fixture.caps.interfaces = []; });
  await page.locator('[data-act="refresh"]').click();
  await expect(page.getByTestId('portal_actual')).toHaveText('Żaden interfejs węzła');
  await expect(page.getByTestId('portal_exposure')).toHaveText('Nie zmierzono');
});

test('mockup N19b przy tym samym viewport', async ({ page }) => {
  await fs.access(path.join(design, 'mockups/tentanas/n19-target.html'));
  await page.goto(`${base}/mockups/tentanas/n19-target.html`);
  await page.locator('.screen').nth(1).scrollIntoViewIfNeeded();
  await page.screenshot({ path: path.join(artifacts, 'n19-mockup-viewport.png') });
  await page.locator('.screen').nth(1).screenshot({ path: path.join(artifacts, 'n19-mockup-desktop.png') });
});

test('N19a: zdrowe iSCSI, sesje i zapis allowlisty bez przepięcia', async ({ page }) => {
  await openDetail(page, { protocol: 'iscsi', sessionsKnown: true, sessions: 2 });
  await expect(page.getByTestId('target-sessions-count')).toHaveText('2');
  await expect(page.getByTestId('portal_actual')).toHaveText('storage0');
  await expect(page.locator('#nas-td-sessions')).toContainText('10.10.0.21');
  await page.screenshot({ path: path.join(artifacts, 'n19a-iscsi.png'), animations: 'disabled' });
  await page.locator('.nas-target-detail details summary').click();
  await page.locator('#nas-td-initiators textarea').fill('iqn.1994-05.com.redhat:vmhost-02');
  await page.locator('.nas-target-detail [data-act="save"]').click();
  await expect.poll(() => page.evaluate(() => window.jobId)).toBe('job-portal');
  const payload = await page.evaluate(() => window.calls.find((call) => call.kind.includes('Update')).payload);
  expect(payload.portals).toEqual([]);
  expect(payload.repickPortal).toBeUndefined();
  expect(payload.initiators).toEqual(['iqn.1994-05.com.redhat:vmhost-02']);
});

test('N19c: potwierdzenie nazwy, błąd pozostawia dialog, ponowienie usuwa eksport', async ({ page }) => {
  await openDetail(page, { protocol: 'iscsi', sessionsKnown: true, sessions: 2, deleteFailure: true });
  await page.locator('.nas-target-detail [data-act="delete"]').click();
  const confirm = page.locator('[data-action="confirm"]');
  await expect(confirm).toHaveAttribute('disabled', '');
  await expect(page.locator('.loss-list')).toContainText('tank/vm-store');
  await expect(page.locator('.loss-list')).toContainText('2');
  await expect(page.locator('.loss-list')).toContainText('iqn.2026-09.local.tentaflow:helios.vm-store');
  await expect(page.locator('.loss-list')).toContainText('Allowlista (2 initiatorów)');
  await expect(page.locator('.loss-list')).toContainText('ustawienia uwierzytelniania');
  await expect(page.locator('.nas-target-delete .explain-box')).toContainText('Snapshoty również pozostają');
  await page.locator('#nas-retype input').fill('vm-stor');
  await expect(confirm).toHaveAttribute('disabled', '');
  await page.locator('#nas-retype input').fill('vm-store');
  await expect(confirm).not.toHaveAttribute('disabled', '');
  await expect(page.getByTestId('target-sessions-count')).toHaveCount(1);
  await page.screenshot({ path: path.join(artifacts, 'n19c-delete.png'), animations: 'disabled' });
  await confirm.click();
  await expect(page.locator('#nas-retype-error')).toContainText('Jądro odmówiło');
  await page.evaluate(() => { window.fixture.deleteFailure = false; });
  await confirm.click();
  await expect.poll(() => page.evaluate(() => window.jobId)).toBe('job-delete');
  await expect(page.locator('#nas-retype')).toHaveCount(0);
});

for (const sessionsKnown of [true, false]) {
  test(`usunięcie: zero sesji, pomiar ${sessionsKnown}, anuluj nie wysyła mutacji`, async ({ page }) => {
    await openDetail(page, { sessionsKnown });
    await page.locator('.nas-target-detail [data-act="delete"]').click();
    await expect(page.locator('.loss-list')).toContainText('fast/scratch');
    const unknown = page.locator('.loss-list').filter({ hasText: 'nieznana' });
    await expect(unknown).toHaveCount(sessionsKnown ? 0 : 1);
    await page.locator('[data-action="cancel"]').last().click();
    await expect(page.locator('#nas-retype')).toHaveCount(0);
    expect(await page.evaluate(() => window.calls.filter((call) => call.kind.includes('Delete')))).toEqual([]);
  });
}

test('strona N19: powrót N12 zachowuje filtr; reload i zmieniony deep link odtwarzają target', async ({ page }) => {
  await openDetail(page);
  await expect(page).toHaveURL(/target=scratch/);
  await expect(page.locator('#nas-tabs')).toHaveAttribute('value', 'shares');
  await expect(page.locator('#nas-tabs').getByRole('tab', { name: /Udostępnianie/ })).toHaveAttribute('aria-selected', 'true');
  await page.locator('[data-act="back"]').click();
  await expect(page.locator('#nas-sh-search')).toBeVisible();
  await page.locator('#nas-sh-search input').fill('scratch');
  await page.locator('#nas-sh-filter [data-value="nvmet"]').click();
  await expect.poll(() => page.evaluate(() => window.screenUnderTest.sharesQuery)).toBe('scratch');
  await page.locator('#nas-tg-table tbody tr').first().click();
  await expect(page.getByTestId('portal_configured')).toHaveText('10.10.0.7:4420');
  await page.locator('[data-act="back"]').click();
  await expect(page.locator('#nas-sh-search input')).toHaveValue('scratch');
  await expect(page.locator('#nas-sh-filter')).toHaveAttribute('value', 'nvmet');
  await page.locator('#nas-tg-table tbody tr').first().click();
  await page.reload();
  await expect(page.getByTestId('portal_configured')).toHaveText('10.10.0.7:4420');
  await page.evaluate(() => { location.hash = '#/tentanas?node=other&tab=shares&target=scratch'; });
  await expect.poll(() => page.evaluate(() => window.screenUnderTest.nodeId)).toBe('other');
  await expect(page.getByTestId('portal_configured')).toHaveText('10.10.0.7:4420');
});

test('alert z innej zakładki otwiera nazwany target i zaznacza Udostępnianie', async ({ page }) => {
  await openDetail(page);
  await page.locator('#nas-tabs').getByRole('tab', { name: 'Zadania' }).click();
  await expect(page.locator('#nas-tabs').getByRole('tab', { name: 'Zadania' })).toHaveAttribute('aria-selected', 'true');
  const previous = await page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasTargetGetRequest').length);
  await page.evaluate(() => window.screenUnderTest.switchTab('shares', { target: 'scratch' }));
  await expect(page.locator('.nas-target-page-head h2')).toHaveText('scratch');
  await expect(page).toHaveURL(/target=scratch/);
  await expect.poll(() => page.evaluate(() => window.calls.filter((call) => call.kind === 'tentaNasTargetGetRequest').length)).toBe(previous + 1);
  await expect(page.locator('#nas-tabs').getByRole('tab', { name: /Udostępnianie/ })).toHaveAttribute('aria-selected', 'true');
});

test('draft allowlisty przeżywa odświeżenie i anulowanie kreatora, usunięcie wymaga Zapisz', async ({ page }) => {
  await openDetail(page, { protocol: 'iscsi', sessionsKnown: true, sessions: 2 });
  await page.locator('#nas-td-hosts tbody tr').first().getByRole('button').click();
  await expect(page.locator('#nas-td-hosts tbody tr')).toHaveCount(1);
  await expect(page.getByTestId('initiators-draft-hint')).toBeVisible();
  await page.locator('[data-act="refresh"]').click();
  await expect(page.locator('#nas-td-hosts tbody tr')).toHaveCount(1);
  await page.locator('[data-act="edit"]').click();
  await page.locator('[data-wizard-cancel]').click();
  await expect(page.locator('#nas-td-hosts tbody tr')).toHaveCount(1);
  expect(await page.evaluate(() => window.calls.filter((call) => call.kind.includes('Update')))).toEqual([]);
  await page.locator('[data-act="save"]').click();
  await expect.poll(() => page.evaluate(() => window.calls.filter((call) => call.kind.includes('Update')).length)).toBe(1);
  expect(await page.evaluate(() => window.calls.find((call) => call.kind.includes('Update')).payload.initiators)).toEqual(['iqn.1994-05.com.redhat:vmhost-02']);
});

test('allowlista pokazuje aktualną metodę auth i target współdzielący identyfikator', async ({ page }) => {
  await openDetail(page);
  await page.evaluate(() => { window.fixture.siblings = [{ ...window.fixture.target, targetId: 'sibling', name: 'vm-backup' }]; });
  await page.locator('[data-act="refresh"]').click();
  await expect(page.locator('#nas-td-hosts tbody')).toContainText('vm-backup');
  await expect(page.locator('#nas-td-hosts tbody')).toContainText('DH-HMAC-CHAP');
});

test('pisanie draftu podczas oczekiwania na odświeżenie nie traci zmian', async ({ page }) => {
  await openDetail(page, { protocol: 'iscsi' });
  await page.evaluate(() => {
    const screen = window.screenUnderTest;
    const transport = screen.nas;
    screen.nas = (kind, payload) => kind.includes('TargetGet') ? new Promise((resolve) => { window.releaseRefresh = async () => resolve(await transport(kind, payload)); }) : transport(kind, payload);
  });
  await page.locator('[data-act="refresh"]').click();
  await page.locator('.nas-target-detail details summary').click();
  await page.locator('#nas-td-initiators textarea').fill('iqn.1994-05.com.redhat:draft');
  await page.evaluate(() => window.releaseRefresh());
  await expect(page.locator('#nas-td-hosts tbody')).toContainText('iqn.1994-05.com.redhat:draft');
});

for (const surface of ['wizard', 'delete', 'create']) {
  test(`N12: stary ${surface} nie wysyła mutacji po zmianie węzła`, async ({ page }) => {
    await openDetail(page);
    await page.locator('[data-act="back"]').click();
    await page.evaluate(async () => { window.screenUnderTest.withSudo = (await import('/js/modules/tentanas.js')).default.withSudo; });
    if (surface === 'create') {
      await page.locator('[data-act="create-target"]').click();
      await page.locator('#nas-tw-name input').fill('new-volume');
      await page.locator('[data-wizard-next]').click();
      await page.locator('#nas-tw-auth [data-value="none"]').click();
      await page.locator('[data-wizard-next]').click();
    } else if (surface === 'wizard') {
      await page.locator('#nas-tg-table [data-act="edit"]').click();
      await page.locator('[data-wizard-next]').click();
    } else {
      await page.locator('#nas-tg-table [data-act="delete"]').click();
      await page.locator('#nas-retype input').fill('scratch');
    }
    await page.locator('#nas-node-select select').selectOption('other');
    await expect(page.locator('#nas-head-sub')).toContainText('other');
    await page.locator(surface === 'delete' ? 'tf-window [data-action="confirm"]' : '[data-wizard-next]').click();
    await expect(page.locator('.toast')).toContainText('Kontekst');
    expect(await page.evaluate(() => window.calls.filter((call) => /CreateRequest|UpdateRequest|DeleteRequest|ArmRequest/.test(call.kind)))).toEqual([]);
  });
}

for (const surface of ['wizard', 'delete']) {
  test(`utrata kontekstu detalu blokuje zapis otwartego ${surface}`, async ({ page }) => {
    await openDetail(page);
    await page.evaluate(async () => {
      const screen = window.screenUnderTest;
      screen.withSudo = (await import('/js/modules/tentanas.js')).default.withSudo;
      screen.environment = { elevation: { mode: 'helper', helperState: 'ok' } };
    });
    if (surface === 'wizard') {
      await page.locator('[data-act="edit"]').click();
      await page.locator('[data-wizard-next]').click();
    } else {
      await page.locator('[data-act="delete"]').click();
      await page.locator('#nas-retype input').fill('scratch');
    }
    await page.evaluate(() => window.screenUnderTest.openTarget(null));
    await page.locator(surface === 'wizard' ? '[data-wizard-next]' : 'tf-window [data-action="confirm"]').click();
    await expect(page.locator('.toast')).toContainText('Kontekst');
    expect(await page.evaluate(() => window.calls.filter((call) => /UpdateRequest|DeleteRequest|ArmRequest/.test(call.kind)))).toEqual([]);
  });
}

test('spóźnione Get/List starego targetu nie nadpisują nowego węzła i detalu', async ({ page }) => {
  await openDetail(page);
  await page.evaluate(() => {
    const screen = window.screenUnderTest;
    const transport = screen.nas;
    const pending = [];
    screen.nas = (kind, payload) => {
      if (pending.length < 2) return new Promise((resolve) => pending.push({ resolve, kind }));
      return transport(kind, payload);
    };
    screen.openTarget('scratch');
    window.fixture.target = { ...window.fixture.target, targetId: 'new-target', name: 'new-target' };
    screen.nodeId = 'other';
    screen.openTarget('new-target');
    window.releaseOld = () => pending.forEach(({ resolve, kind }) => resolve(kind.includes('Get') ? { target: { ...window.fixture.target, name: 'OLD-DATA' }, sessions: [] } : { targets: [], capabilities: window.fixture.caps }));
  });
  await expect(page.locator('.nas-target-page-head h2')).toHaveText('new-target');
  await page.evaluate(() => window.releaseOld());
  await expect(page.locator('.nas-target-page-head h2')).toHaveText('new-target');
  await expect(page.locator('.nas-target-detail')).not.toContainText('OLD-DATA');
});

for (const changeNode of [false, true]) {
  test(`rzeczywiste sudo remember: zmiana węzła ${changeNode} blokuje arming i mutację`, async ({ page }) => {
    await openDetail(page);
    await page.evaluate(async () => {
      const screen = window.screenUnderTest;
      screen.withSudo = (await import('/js/modules/tentanas.js')).default.withSudo;
      screen.environment = { elevation: { mode: 'unarmed', ttlSecs: 900, coreUser: 'tentaflow' } };
      screen.refreshHeader = async () => {};
      const transport = screen.nas;
      screen.nas = (kind, payload) => {
        if (kind === 'tentaNasElevationArmRequest') { window.calls.push({ kind, payload }); return Promise.resolve({}); }
        return transport(kind, payload);
      };
    });
    await page.locator('[data-act="save"]').click();
    await page.locator('#nas-sudo-pass input').fill('test-password');
    await page.locator('#nas-sudo-remember').click();
    if (changeNode) await page.evaluate(() => { window.screenUnderTest.nodeId = 'other'; window.screenUnderTest.openTarget(null); });
    await page.locator('tf-window [data-action="confirm"]').click();
    await expect(page.locator('#nas-sudo-pass')).toHaveCount(0);
    if (changeNode) {
      await expect(page.locator('.toast')).toContainText('Kontekst');
      expect(await page.evaluate(() => window.calls.filter((call) => /ArmRequest|UpdateRequest/.test(call.kind)))).toEqual([]);
    } else {
      await expect.poll(() => page.evaluate(() => window.calls.filter((call) => /ArmRequest|UpdateRequest/.test(call.kind)).length)).toBe(2);
    }
  });
}

for (const width of [390, 768]) {
  test(`detal i wybór portalu w viewport ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 844 });
    await openDetail(page);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    const selectedTab = page.locator('#nas-tabs [role="tab"][aria-selected="true"]');
    await expect.poll(async () => {
      const tab = await selectedTab.boundingBox();
      return tab.x >= 0 && tab.x + tab.width <= width;
    }).toBe(true);
    for (const control of ['#nas-node-select', '[data-act="export-config"]', '[data-act="reprobe"]']) {
      const box = await page.locator(control).boundingBox();
      expect(box.x).toBeGreaterThanOrEqual(0);
      expect(box.x + box.width).toBeLessThanOrEqual(width);
      expect(box.width).toBeGreaterThan(120);
      const hit = await page.locator(control).evaluate((element) => {
        const box = element.getBoundingClientRect();
        return element.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2));
      });
      expect(hit).toBe(true);
    }
    await page.screenshot({ path: path.join(artifacts, `n19-detail-${width}.png`), animations: 'disabled' });
    await page.locator('[data-act="repick-portal"]').click();
    await expect(page.locator('.install-step-body')).toBeVisible();
    await page.locator('#nas-tw-iface select').selectOption('storage0');
    await page.locator('[data-wizard-next]').click();
    await expect(page.locator('.install-step-body')).toContainText('10.10.0.5:4420');
    await page.screenshot({ path: path.join(artifacts, `n19-summary-${width}.png`), animations: 'disabled' });
    const box = await page.locator('tf-window .tf-window').boundingBox();
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(width);
    await page.locator('[data-wizard-cancel]').click();
    await page.locator('#nas-node-select select').selectOption('other');
    await expect(page.locator('#nas-head-sub')).toContainText('other');
  });
}
