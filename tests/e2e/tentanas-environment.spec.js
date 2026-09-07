// =============================================================================
// Plik: tentanas-environment.spec.js
// Opis: Przeglądarkowa kontrola rzeczywistej tabeli Środowisko dla wyników sondy.
//       Dane środowiska są fixture; renderer, komponenty i tłumaczenia są produkcyjne.
// =============================================================================

const { test, expect } = require('@playwright/test');
const fs = require('node:fs/promises');
const path = require('node:path');

const root = path.resolve(__dirname, '../../tentaflow-core/www');
const artifacts = path.resolve(process.env.TENTANAS_E2E_ARTIFACTS || path.resolve(__dirname, '../../../new_apps/reviews/artifacts/T06'));
const labels = {
  pl: ['OK', 'nie zmierzono', 'błąd działania'],
  en: ['OK', 'not measured', 'execution failed'],
  de: ['OK', 'nicht gemessen', 'Ausführung fehlgeschlagen'],
  es: ['OK', 'no medido', 'fallo de ejecución'],
  fr: ['OK', 'non mesuré', 'échec d’exécution'],
};

test.use({ viewport: { width: 1440, height: 1080 } });

for (const lang of Object.keys(labels)) {
  test(`Środowisko: pomiar ok/unknown/broken, język ${lang}`, async ({ page }, testInfo) => {
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
    const index = await fs.readFile(path.join(root, 'index.html'), 'utf8');
    const sprite = index.match(/<svg[^>]*(?:data-role="sprite"|aria-hidden="true")[\s\S]*?<\/svg>/)?.[0];
    expect(sprite).toContain('id="i-target"');
    await page.route('http://127.0.0.1:18763/**', async (route) => {
      const pathname = new URL(route.request().url()).pathname;
      if (pathname === '/') {
        await route.fulfill({ contentType: 'text/html', body: `<!doctype html><html><head><meta charset="utf-8"><link rel="icon" href="data:,">${['controls', 'style', 'compat', 'install-wizard', 'tentanas'].map((name) => `<link rel="stylesheet" href="/css/${name}.css">`).join('')}</head><body>${sprite}<main class="nas-root" id="environment"></main></body></html>` });
        return;
      }
      const file = path.resolve(root, '.' + decodeURIComponent(pathname));
      if (!file.startsWith(root + path.sep)) { await route.fulfill({ status: 403 }); return; }
      await route.fulfill({ path: file });
    });
    await page.goto('http://127.0.0.1:18763/');
    await page.evaluate(async (language) => {
      localStorage.setItem('tentaflow_lang', language);
      const { I18n } = await import('/js/i18n.js');
      await I18n.init();
      const { default: screen } = await import('/js/modules/tentanas.js');
      screen.isAdmin = false;
      screen.nodeId = 'helios';
      screen.nodes = [{ nodeId: 'helios', nodeName: 'helios', ramBytes: 137438953472 }];
      screen.nas = async (kind) => {
        if (kind === 'tentaNasArcStatsRequest') return { arc: null };
        throw new Error(`Nieoczekiwane żądanie ${kind}`);
      };
      screen.environment = {
        fullSupport: true, osName: 'Linux', ramBytes: 137438953472,
        elevation: { mode: 'unarmed', helperState: 'absent', coreUser: 'tentaflow', ttlSecs: 900 },
        features: ['ok', 'unknown', 'broken'].map((status) => ({ id: 'snapraid', status, optional: true, binaries: ['snapraid'], packages: [], detail: status === 'unknown' ? 'Nie uzyskano wyniku sondy' : status === 'broken' ? 'Proces sondy zakończony sygnałem' : 'Odczyt zakończony poprawnie' })),
      };
      await screen.drawEnvironment(document.querySelector('#environment'));
    }, lang);
    const chips = page.locator('#nas-feature-table tbody .tf-chip');
    await expect(chips).toHaveCount(3);
    for (const [i, tone] of ['ok', 'warn', 'err'].entries()) {
      await expect(chips.nth(i)).toHaveText(labels[lang][i]);
      await expect(chips.nth(i)).toHaveClass(new RegExp(`\\b${tone}\\b`));
    }
    await expect(page.locator('#nas-feature-table')).not.toContainText('feature_status.');
    await expect(page.locator('#nas-feature-table')).not.toContainText('tentanas.feature.');
    await expect(page.locator('#nas-feature-table')).toContainText('SnapRAID');
    expect(await page.evaluate(async () => (await import('/js/i18n.js')).I18n.t('tentanas.feature.mergerfs'))).toBe('mergerfs');
    if (lang === 'pl') {
      await fs.mkdir(artifacts, { recursive: true });
      await page.screenshot({ path: path.join(artifacts, 'environment-statuses.png'), animations: 'disabled' });
    }
    await testInfo.attach('konsola', { body: JSON.stringify(errors), contentType: 'application/json' });
    expect(errors).toEqual([]);
  });
}
